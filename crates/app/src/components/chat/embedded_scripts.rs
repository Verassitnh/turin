//! Port of upstream/apps/web/src/lib/embeddedScripts.ts.
//!
//! Upstream parses the command with `unbash`. Here tree-sitter-bash parses it, and
//! the walk, the word decoding and the position mapping are written against that
//! tree. The interpreter tables, the program-wrapper rules and the argument rules
//! are upstream's, unchanged. See PORTING.md Deviations for the parser differences.
//!
//! Positions are UTF-8 byte offsets into the command (Rust string slicing), where
//! upstream uses UTF-16 units. `starts[i]..ends[i]` spans the command character that
//! spells `text` character `i`.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::{Node, Parser};
use turin_contracts::base_schemas::js_trim;

use crate::diff::highlight::language_for_path;

/// Code in another language inside a shell command: a `bash -lc "…"` script, a
/// `python3 -c '…'` argument, or a heredoc fed to an interpreter or written to a
/// file. `starts[i]` and `ends[i]` are the span of the command that spells `text`
/// character `i`, so a quoted `\"` maps back to both of its characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedScript {
    pub language: String,
    pub text: String,
    pub starts: Vec<usize>,
    pub ends: Vec<usize>,
}

/// Maximum nesting of shell scripts inside shell scripts (`MAX_DEPTH`).
const MAX_DEPTH: usize = 4;

/// The `inline` predicate: which flags take the next argument (or `=value`) as a script.
#[derive(Clone, Copy)]
enum Inline {
    /// `flagIn(...)`.
    Flags(&'static [&'static str]),
    /// The shell pattern `^-[a-z]*c[a-z]*$`: `-c`, `-lc`, `-ec`, …
    ShellCommandFlag,
}

impl Inline {
    fn matches(self, flag: &str) -> bool {
        match self {
            Self::Flags(flags) => flags.contains(&flag),
            Self::ShellCommandFlag => {
                flag.len() >= 2
                    && flag.starts_with('-')
                    && flag[1..].bytes().all(|byte| byte.is_ascii_lowercase())
                    && flag.contains('c')
            }
        }
    }
}

/// When a command without a script argument reads its script from stdin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StdinScript {
    /// SQL shells read statements from stdin next to their database argument.
    Always,
    WithoutScriptFile,
    Never,
}

#[derive(Clone, Copy)]
struct Interpreter {
    language: &'static str,
    inline: Inline,
    /// Options that consume the next argument.
    value_options: &'static [&'static str],
    /// Options after which the script comes from a file or module instead.
    script_file_options: &'static [&'static str],
    stdin: StdinScript,
    /// The first positional argument holding a script, as in `sqlite3 app.db "select 1"`.
    positional_script: Option<usize>,
    /// SQL shells run every `-c` and every statement argument, in order.
    repeatable: bool,
    subcommand: Option<&'static str>,
    /// PowerShell matches parameters case-insensitively; the lists above are lowercase.
    ignore_case: bool,
}

const NO_OPTIONS: &[&str] = &[];

const SHELL: Interpreter = Interpreter {
    language: "shellscript",
    inline: Inline::ShellCommandFlag,
    value_options: &["-o", "-O", "--rcfile", "--init-file"],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const PYTHON: Interpreter = Interpreter {
    language: "python",
    inline: Inline::Flags(&["-c"]),
    value_options: &["-W", "-X"],
    script_file_options: &["-m"],
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

/// Shared by SQL clients; each names its own script flags, since `-c` and `-f` mean
/// different things to psql and mysql.
const SQL_SHELL: Interpreter = Interpreter {
    language: "sql",
    inline: Inline::Flags(&[]),
    value_options: &[
        "-d",
        "-h",
        "-u",
        "-U",
        "--dbname",
        "--host",
        "--user",
        "--username",
        "-separator",
        "-nullvalue",
        "-init",
    ],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::Always,
    positional_script: None,
    repeatable: true,
    subcommand: None,
    ignore_case: false,
};

const NODE: Interpreter = Interpreter {
    language: "javascript",
    inline: Inline::Flags(&["-e", "--eval", "-p", "--print"]),
    value_options: &[
        "-r",
        "--require",
        "--import",
        "--loader",
        "-C",
        "--conditions",
    ],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const BUN: Interpreter = Interpreter {
    language: "typescript",
    inline: Inline::Flags(&["-e", "--eval", "-p", "--print"]),
    value_options: NO_OPTIONS,
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::Never,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const DENO: Interpreter = Interpreter {
    language: "typescript",
    inline: Inline::Flags(&[]),
    // `--env-file` takes its optional path only as `--env-file=path`.
    value_options: &["-c", "--config", "--import-map", "--ext"],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::Never,
    positional_script: None,
    repeatable: false,
    subcommand: Some("eval"),
    ignore_case: false,
};

const RUBY: Interpreter = Interpreter {
    language: "ruby",
    inline: Inline::Flags(&["-e"]),
    value_options: &["-r", "-I", "-C"],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const PERL: Interpreter = Interpreter {
    language: "perl",
    inline: Inline::Flags(&["-e", "-E"]),
    value_options: NO_OPTIONS,
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const OSASCRIPT: Interpreter = Interpreter {
    language: "applescript",
    inline: Inline::Flags(&["-e"]),
    value_options: &["-l", "-s"],
    script_file_options: NO_OPTIONS,
    stdin: StdinScript::WithoutScriptFile,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: false,
};

const PWSH: Interpreter = Interpreter {
    language: "powershell",
    inline: Inline::Flags(&["-c", "-command"]),
    value_options: &["-executionpolicy", "-workingdirectory", "-windowstyle"],
    script_file_options: &["-f", "-file", "-encodedcommand"],
    stdin: StdinScript::Never,
    positional_script: None,
    repeatable: false,
    subcommand: None,
    ignore_case: true,
};

const PSQL: Interpreter = Interpreter {
    inline: Inline::Flags(&["-c", "--command"]),
    script_file_options: &["-f", "--file"],
    ..SQL_SHELL
};

const MYSQL: Interpreter = Interpreter {
    inline: Inline::Flags(&["-e", "--execute"]),
    ..SQL_SHELL
};

const SQLITE3: Interpreter = Interpreter {
    inline: Inline::Flags(&["-cmd"]),
    positional_script: Some(1),
    ..SQL_SHELL
};

const DUCKDB: Interpreter = Interpreter {
    inline: Inline::Flags(&["-c", "-cmd"]),
    script_file_options: &["-f"],
    positional_script: Some(1),
    ..SQL_SHELL
};

/// `INTERPRETERS[name]`, with upstream's aliases (`powershell`, `mariadb`) and the
/// Python name pattern handled as `interpreterFor` does.
fn interpreter_named(name: &str) -> Option<Interpreter> {
    // `py` is the Windows Python launcher, as in `py -3 -c '…'`.
    if PYTHON_NAME.is_match(name) {
        return Some(PYTHON);
    }
    Some(match name {
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" => SHELL,
        "fish" => Interpreter {
            language: "fish",
            inline: Inline::Flags(&["-c", "--command"]),
            value_options: NO_OPTIONS,
            ..SHELL
        },
        "node" => NODE,
        "bun" => BUN,
        "deno" => DENO,
        "ruby" => RUBY,
        "perl" => PERL,
        "osascript" => OSASCRIPT,
        "pwsh" | "powershell" => PWSH,
        "psql" => PSQL,
        "mysql" | "mariadb" => MYSQL,
        "sqlite3" => SQLITE3,
        "duckdb" => DUCKDB,
        _ => return None,
    })
}

static PYTHON_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^python(?:\d+(?:\.\d+)?)?$|^pypy3?$|^py$").expect("valid regex"));

/// Commands that run the next word as the program, with the options that consume a value.
fn program_wrapper_options(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "env" => &["-u", "--unset", "-C", "--chdir"],
        "sudo" => &["-u", "--user", "-g", "--group", "-C", "-D", "--chdir"],
        "nice" => &["-n", "--adjustment"],
        "nohup" | "command" | "builtin" => NO_OPTIONS,
        "exec" => &["-a"],
        _ => return None,
    })
}

/// Heredoc delimiters that name their language, as in `<<'SQL'`.
fn delimiter_language(delimiter: &str) -> Option<&'static str> {
    Some(match delimiter {
        "py" | "python" => "python",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "sql" => "sql",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sh" | "bash" | "shell" => "shellscript",
        "rb" | "ruby" => "ruby",
        "html" => "html",
        "css" => "css",
        "md" | "markdown" => "markdown",
        "xml" => "xml",
        "diff" | "patch" => "diff",
        _ => return None,
    })
}

/// The scripts embedded in a shell command, outermost first, so a highlighter painting
/// them in order lets a nested script override the one containing it. Returns nothing
/// for a command that does not parse cleanly; where a broken quote ends cannot be
/// trusted.
pub fn embedded_scripts(command: &str) -> Vec<EmbeddedScript> {
    let root = Level::root(command);
    scripts_in(&root, 0)
}

/// One level of shell text, with the command span of each of its characters.
struct Level {
    chars: Vec<char>,
    /// Byte offset in `source` where each character starts, plus `source.len()`.
    char_starts: Vec<usize>,
    source: String,
    /// Command byte span of each character.
    spans: Vec<(usize, usize)>,
}

impl Level {
    fn new(source: String, spans: Vec<(usize, usize)>) -> Self {
        let mut chars = Vec::with_capacity(spans.len());
        let mut char_starts = Vec::with_capacity(spans.len() + 1);
        for (byte, character) in source.char_indices() {
            char_starts.push(byte);
            chars.push(character);
        }
        char_starts.push(source.len());
        debug_assert_eq!(chars.len(), spans.len());
        Self {
            chars,
            char_starts,
            source,
            spans,
        }
    }

    fn root(command: &str) -> Self {
        let spans = command
            .char_indices()
            .map(|(byte, character)| (byte, byte + character.len_utf8()))
            .collect();
        Self::new(command.to_owned(), spans)
    }

    /// The character index whose text starts at `byte` (or the one after it, for a
    /// byte at the end of the text).
    fn char_at(&self, byte: usize) -> usize {
        self.char_starts.partition_point(|&start| start < byte)
    }

    /// The character indices a node covers.
    fn chars_of(&self, node: Node<'_>) -> Range<usize> {
        self.char_at(node.start_byte())..self.char_at(node.end_byte())
    }

    fn text_of(&self, node: Node<'_>) -> &str {
        &self.source[node.start_byte()..node.end_byte()]
    }
}

/// Characters built from one level, each with the level character range it came from.
#[derive(Default)]
struct Mapped {
    chars: Vec<char>,
    /// Level character index of the first and (exclusive) last character each output
    /// character spans.
    starts: Vec<usize>,
    ends: Vec<usize>,
}

impl Mapped {
    fn push(&mut self, character: char, start: usize, end: usize) {
        self.chars.push(character);
        self.starts.push(start);
        self.ends.push(end);
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// `sliceMapped`: the characters from `from` on.
    fn sliced(self, from: usize) -> Self {
        if from == 0 {
            return self;
        }
        Self {
            chars: self.chars[from..].to_vec(),
            starts: self.starts[from..].to_vec(),
            ends: self.ends[from..].to_vec(),
        }
    }
}

/// `addRaw`: each character maps to itself.
fn add_raw(out: &mut Mapped, level: &Level, range: Range<usize>) {
    for index in range {
        out.push(level.chars[index], index, index + 1);
    }
}

/// `addEscaped`: resolves backslash escapes. Inside double quotes only `$`, backtick,
/// `"`, `\` and newline are escapable; a backslash-newline joins lines and spells
/// nothing.
fn add_escaped(out: &mut Mapped, level: &Level, range: Range<usize>, double_quoted: bool) {
    let mut index = range.start;
    while index < range.end {
        let character = level.chars[index];
        let next = (index + 1 < range.end).then(|| level.chars[index + 1]);
        match next {
            Some(next) if character == '\\' && (!double_quoted || "$`\"\\\n".contains(next)) => {
                if next != '\n' {
                    out.push(next, index, index + 2);
                }
                index += 2;
            }
            _ => {
                out.push(character, index, index + 1);
                index += 1;
            }
        }
    }
}

/// The characters between a quoted node's delimiters (`'…'`, `"…"`, `$'…'`).
fn quoted_interior(level: &Level, node: Node<'_>, opening: usize) -> Range<usize> {
    let chars = level.chars_of(node);
    chars.start + opening..chars.end - 1
}

/// Whether an ANSI-C string has escapes. Those do not map character for character, as
/// upstream's `value !== text` check finds, so the string is not mapped.
fn ansi_c_has_escapes(level: &Level, node: Node<'_>) -> bool {
    level.chars[quoted_interior(level, node, 2)].contains(&'\\')
}

/// Decodes one word-like node into `out`. `None` when the spelling cannot be mapped.
fn add_node(out: &mut Mapped, level: &Level, node: Node<'_>) -> Option<()> {
    match node.kind() {
        "string" => add_parts(out, level, node, quoted_interior(level, node, 1), true),
        "raw_string" => {
            add_raw(out, level, quoted_interior(level, node, 1));
            Some(())
        }
        "ansi_c_string" => {
            if ansi_c_has_escapes(level, node) {
                return None;
            }
            add_raw(out, level, quoted_interior(level, node, 2));
            Some(())
        }
        // `$"…"`: only the string is the text; the `$` is not.
        "translated_string" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "string" {
                    add_node(out, level, child)?;
                }
            }
            Some(())
        }
        _ => add_parts(out, level, node, level.chars_of(node), false),
    }
}

/// The parts of a node, in order: literal text between the named children (escapes
/// resolved per `double_quoted`), quoted children decoded, and expansions kept as
/// written, as upstream does for everything it does not map.
fn add_parts(
    out: &mut Mapped,
    level: &Level,
    node: Node<'_>,
    range: Range<usize>,
    double_quoted: bool,
) -> Option<()> {
    let mut position = range.start;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // Plain text: left to the literal gap around it.
            "string_content" => continue,
            "string" | "raw_string" | "ansi_c_string" | "translated_string" => {
                let child_range = level.chars_of(child);
                add_escaped(out, level, position..child_range.start, double_quoted);
                add_node(out, level, child)?;
                position = child_range.end;
            }
            "word" | "concatenation" | "command_name" => {
                let child_range = level.chars_of(child);
                add_escaped(out, level, position..child_range.start, double_quoted);
                add_parts(out, level, child, child_range.clone(), double_quoted)?;
                position = child_range.end;
            }
            _ => {
                let child_range = level.chars_of(child);
                add_escaped(out, level, position..child_range.start, double_quoted);
                add_raw(out, level, child_range.clone());
                position = child_range.end;
            }
        }
    }
    add_escaped(out, level, position..range.end, double_quoted);
    Some(())
}

/// A word's text with each character's source span: quotes removed and escapes
/// resolved, the way the program receives it. Expansions such as `$HOME` stay as
/// written. `None` for spellings that cannot be mapped.
fn word_text(level: &Level, node: Node<'_>) -> Option<Mapped> {
    let mut out = Mapped::default();
    add_node(&mut out, level, node)?;
    Some(out)
}

/// The decoded value of a word, as the program sees it. A spelling `word_text` cannot
/// map falls back to its source text.
fn word_value(level: &Level, node: Node<'_>) -> String {
    word_text(level, node).map_or_else(|| level.text_of(node).to_owned(), |mapped| mapped.text())
}

/// A command argument or name.
#[derive(Clone, Copy)]
struct Arg<'t> {
    node: Node<'t>,
}

/// A simple command: its name, its arguments, and the redirections that apply to it.
struct Command<'t> {
    name: Arg<'t>,
    args: Vec<Arg<'t>>,
    redirects: Vec<Redirect<'t>>,
}

enum Redirect<'t> {
    HereString {
        descriptor: Option<u64>,
        target: Arg<'t>,
    },
    HereDoc {
        descriptor: Option<u64>,
        delimiter: Option<String>,
        body: Option<Node<'t>>,
    },
    File {
        operator: String,
        descriptor: Option<u64>,
        target: Option<String>,
    },
}

/// `isDescriptor`: a redirection with no descriptor, or with `fd`, targets `fd`.
fn is_descriptor(descriptor: Option<u64>, fd: u64) -> bool {
    descriptor.is_none_or(|value| value == fd)
}

fn descriptor_of(node: Node<'_>, level: &Level) -> Option<u64> {
    node.child_by_field_name("descriptor")
        .and_then(|descriptor| level.text_of(descriptor).parse().ok())
}

/// Every simple command in the tree, including those inside `$(…)`, loops and
/// subshells, in document order.
fn collect_commands<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    if node.kind() == "command" {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_commands(child, out);
    }
}

/// The heredoc body a `heredoc_redirect` reads. The body is a child of the redirect
/// or, in some grammar shapes, the next body in document order.
fn heredoc_body_of<'t>(
    redirect: Node<'t>,
    bodies: &mut std::slice::Iter<'_, Node<'t>>,
) -> Option<Node<'t>> {
    let mut cursor = redirect.walk();
    let child = redirect
        .named_children(&mut cursor)
        .find(|child| child.kind() == "heredoc_body");
    child.or_else(|| bodies.next().copied())
}

/// Every `heredoc_body` in document order.
fn collect_heredoc_bodies<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    if node.kind() == "heredoc_body" {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_heredoc_bodies(child, out);
    }
}

/// Redirections that apply to a command: its own, and those of the statement it is the
/// body of.
fn redirects_of<'t>(
    command: Node<'t>,
    level: &Level,
    bodies: &mut std::slice::Iter<'_, Node<'t>>,
) -> Vec<Redirect<'t>> {
    let mut nodes = Vec::new();
    let mut cursor = command.walk();
    nodes.extend(command.children_by_field_name("redirect", &mut cursor));
    if let Some(parent) = command.parent().filter(|parent| {
        parent.kind() == "redirected_statement"
            && parent.child_by_field_name("body") == Some(command)
    }) {
        let mut cursor = parent.walk();
        nodes.extend(parent.children_by_field_name("redirect", &mut cursor));
    }
    nodes
        .into_iter()
        .filter_map(|redirect| redirect_of(redirect, level, bodies))
        .collect()
}

fn redirect_of<'t>(
    redirect: Node<'t>,
    level: &Level,
    bodies: &mut std::slice::Iter<'_, Node<'t>>,
) -> Option<Redirect<'t>> {
    let descriptor = descriptor_of(redirect, level);
    match redirect.kind() {
        "herestring_redirect" => {
            let target = redirect.named_children(&mut redirect.walk()).last()?;
            Some(Redirect::HereString {
                descriptor,
                target: Arg { node: target },
            })
        }
        "heredoc_redirect" => {
            let delimiter = redirect
                .named_children(&mut redirect.walk())
                .find(|child| child.kind() == "heredoc_start")
                .map(|start| strip_quotes(level.text_of(start)).to_owned());
            Some(Redirect::HereDoc {
                descriptor,
                delimiter,
                body: heredoc_body_of(redirect, bodies),
            })
        }
        "file_redirect" => {
            let operator = {
                let mut cursor = redirect.walk();
                redirect
                    .children(&mut cursor)
                    .find(|child| !child.is_named())
                    .map_or_else(String::new, |token| level.text_of(token).to_owned())
            };
            let target = redirect
                .child_by_field_name("destination")
                .map(|destination| word_value(level, destination));
            Some(Redirect::File {
                operator,
                descriptor,
                target,
            })
        }
        _ => None,
    }
}

/// The program name, as upstream reads it. A path with backslashes is taken from its
/// spelling: bash reads `C:\Python312\python.exe` unquoted as escapes, and Windows meant
/// a path.
fn program_name(level: &Level, arg: Arg<'_>) -> String {
    let raw = level.text_of(arg.node);
    let path = if raw.contains('\\') {
        strip_one_quote(strip_one_trailing_quote(raw)).to_owned()
    } else {
        word_value(level, arg.node)
    };
    let last = path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    last.strip_suffix(".exe").unwrap_or(&last).to_owned()
}

/// A delimiter token without its quotes: `'PY'` is `PY`.
fn strip_quotes(text: &str) -> &str {
    strip_one_trailing_quote(strip_one_quote(text))
}

/// `replace(/^["']/, "")`.
fn strip_one_quote(text: &str) -> &str {
    text.strip_prefix(['"', '\'']).unwrap_or(text)
}

/// `replace(/["']$/, "")`.
fn strip_one_trailing_quote(text: &str) -> &str {
    text.strip_suffix(['"', '\'']).unwrap_or(text)
}

/// Skips `env FOO=1`, `sudo -u bob` and similar to the word naming the real program.
fn program_index(level: &Level, words: &[Arg<'_>]) -> usize {
    static ASSIGNMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*=").expect("valid regex"));
    let mut index = 0;
    while index < words.len() {
        let name = program_name(level, words[index]);
        let Some(value_options) = program_wrapper_options(&name) else {
            return index;
        };
        index += 1;
        let mut options_ended = false;
        while index < words.len() {
            let value = word_value(level, words[index].node);
            let is_option = !options_ended && value.starts_with('-');
            if is_option && value == "--" {
                options_ended = true;
            }
            // `env -- FOO=1 cmd` still takes assignments after the options end.
            if is_option || ASSIGNMENT.is_match(&value) {
                index += if is_option && value_options.contains(&value.as_str()) {
                    2
                } else {
                    1
                };
            } else {
                break;
            }
        }
    }
    index
}

/// A script argument. `in_value` marks a script that follows the `=` of `--eval=…`.
struct ScriptArg<'t> {
    arg: Arg<'t>,
    in_value: bool,
}

/// The script arguments, and whether the script comes from a file instead.
struct InterpreterArgs<'t> {
    scripts: Vec<ScriptArg<'t>>,
    script_from_file: bool,
}

fn interpreter_args<'t>(
    level: &Level,
    interpreter: &Interpreter,
    args: &[Arg<'t>],
) -> InterpreterArgs<'t> {
    let mut positionals: Vec<(Arg<'t>, String)> = Vec::new();
    let mut scripts = Vec::new();
    let whole = |arg: Arg<'t>| ScriptArg {
        arg,
        in_value: false,
    };
    let mut options_ended = false;
    let mut script_from_file = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        let value = word_value(level, arg.node);
        let flag = if interpreter.ignore_case {
            value.to_lowercase()
        } else {
            value.clone()
        };
        if !options_ended && flag == "--" {
            options_ended = true;
            index += 1;
            continue;
        }
        if !options_ended && flag.starts_with('-') && flag != "-" {
            // `--eval=code` and `--file=path` carry their value in the same word.
            let (name, option_value): (&str, Option<&str>) = if flag.starts_with("--") {
                match flag.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (flag.as_str(), None),
                }
            } else {
                (flag.as_str(), None)
            };
            if interpreter.script_file_options.contains(&name) {
                if !interpreter.repeatable {
                    return InterpreterArgs {
                        scripts,
                        script_from_file: true,
                    };
                }
                // SQL shells run `-f` files and `-c` statements alike, in order.
                script_from_file = true;
                if option_value.is_none() {
                    index += 1;
                }
                index += 1;
                continue;
            }
            if interpreter.inline.matches(name) {
                let next = args.get(index + 1).copied();
                if option_value.is_some() {
                    scripts.push(ScriptArg {
                        arg,
                        in_value: true,
                    });
                } else if let Some(next) = next {
                    scripts.push(whole(next));
                }
                if !interpreter.repeatable {
                    return InterpreterArgs {
                        scripts,
                        script_from_file: false,
                    };
                }
                if option_value.is_none() {
                    index += 1;
                }
                index += 1;
                continue;
            }
            if option_value.is_none() && interpreter.value_options.contains(&flag.as_str()) {
                index += 1;
            }
            index += 1;
            continue;
        }
        positionals.push((arg, value));
        // An interpreter stops reading its own options at the script file.
        if interpreter.stdin == StdinScript::WithoutScriptFile && interpreter.subcommand.is_none() {
            break;
        }
        index += 1;
    }

    if let Some(subcommand) = interpreter.subcommand {
        if positionals.first().map(|(_, value)| value.as_str()) != Some(subcommand) {
            return InterpreterArgs {
                scripts,
                script_from_file: true,
            };
        }
        return InterpreterArgs {
            scripts: positionals
                .iter()
                .skip(1)
                .take(1)
                .map(|(arg, _)| whole(*arg))
                .collect(),
            script_from_file: false,
        };
    }
    if let Some(position) = interpreter.positional_script {
        scripts.extend(
            positionals
                .iter()
                .skip(position)
                .map(|(arg, _)| whole(*arg)),
        );
    }
    // SQL shells take databases as positional arguments, not script files.
    if interpreter.stdin == StdinScript::Always {
        return InterpreterArgs {
            scripts,
            script_from_file,
        };
    }
    let first = positionals.first();
    InterpreterArgs {
        scripts,
        script_from_file: first.is_some_and(|(_, value)| value != "-"),
    }
}

/// The language of the file `cat > file` or `tee file` writes its stdin to.
fn written_file_language(
    name: &str,
    command: &Command<'_>,
    args: &[Arg<'_>],
    level: &Level,
) -> Option<String> {
    let path: Option<String> = if name == "cat" {
        let mut path = None;
        for redirect in &command.redirects {
            if let Redirect::File {
                operator,
                descriptor,
                target,
            } = redirect
                && [">", ">>", ">|", "&>", "&>>"].contains(&operator.as_str())
                && is_descriptor(*descriptor, 1)
            {
                path = target.clone();
            }
        }
        path
    } else if name == "tee" {
        args.iter()
            .map(|arg| word_value(level, arg.node))
            .find(|value| !value.starts_with('-'))
    } else {
        None
    };
    match language_for_path(&path?) {
        "text" => None,
        "bash" => Some("shellscript".to_owned()),
        other => Some(other.to_owned()),
    }
}

/// The scripts a single command carries: its interpreter's scripts, and the heredocs
/// and herestrings it feeds to stdin.
fn command_scripts(level: &Level, command: &Command<'_>) -> Vec<(String, Mapped)> {
    let words: Vec<Arg<'_>> = std::iter::once(command.name)
        .chain(command.args.iter().copied())
        .collect();
    let index = program_index(level, &words);
    let Some(program) = words.get(index).copied() else {
        return Vec::new();
    };
    let name = program_name(level, program);
    let interpreter = interpreter_named(&name);
    let args = &words[index + 1..];
    let mut scripts: Vec<(String, Mapped)> = Vec::new();

    let mut reads_stdin_script = false;
    if let Some(interpreter) = interpreter {
        let found = interpreter_args(level, &interpreter, args);
        for script in &found.scripts {
            let Some(mapped) = word_text(level, script.arg.node) else {
                continue;
            };
            let start = if script.in_value {
                // JS `indexOf("=") + 1`; no `=` gives 0.
                mapped
                    .chars
                    .iter()
                    .position(|&c| c == '=')
                    .map_or(0, |at| at + 1)
            } else {
                0
            };
            scripts.push((interpreter.language.to_owned(), mapped.sliced(start)));
        }
        reads_stdin_script = interpreter.stdin != StdinScript::Never
            && found.scripts.is_empty()
            && !found.script_from_file;
    }

    let stdin_language = if reads_stdin_script {
        interpreter.map(|interpreter| interpreter.language.to_owned())
    } else {
        None
    };
    let written_language = written_file_language(&name, command, args, level);
    for redirect in &command.redirects {
        match redirect {
            Redirect::HereString { descriptor, target } => {
                if let (true, Some(language), Some(mapped)) = (
                    is_descriptor(*descriptor, 0),
                    &stdin_language,
                    word_text(level, target.node),
                ) {
                    scripts.push((language.clone(), mapped));
                }
            }
            Redirect::HereDoc {
                descriptor,
                delimiter,
                body,
            } => {
                let to_stdin = is_descriptor(*descriptor, 0);
                let language = (if to_stdin {
                    stdin_language.clone().or_else(|| written_language.clone())
                } else {
                    None
                })
                .or_else(|| {
                    delimiter
                        .as_deref()
                        .and_then(|delimiter| delimiter_language(&delimiter.to_lowercase()))
                        .map(str::to_owned)
                });
                if let Some(language) = language {
                    scripts.push((language, heredoc_text(level, *body)));
                }
            }
            Redirect::File { .. } => {}
        }
    }
    scripts
}

/// A heredoc body as written; the interpreter reads its characters one to one.
fn heredoc_text(level: &Level, body: Option<Node<'_>>) -> Mapped {
    let mut out = Mapped::default();
    if let Some(body) = body {
        add_raw(&mut out, level, level.chars_of(body));
    }
    out
}

/// `scriptsIn`: the scripts in `level`, depth-first, outermost first.
fn scripts_in(level: &Level, depth: usize) -> Vec<EmbeddedScript> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("tree-sitter-bash grammar loads");
    let Some(tree) = parser.parse(&level.source, None) else {
        return Vec::new();
    };
    if tree.root_node().has_error() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut command_nodes = Vec::new();
    collect_commands(tree.root_node(), &mut command_nodes);
    let mut body_nodes = Vec::new();
    collect_heredoc_bodies(tree.root_node(), &mut body_nodes);
    let mut bodies = body_nodes.iter();
    for command_node in command_nodes {
        let Some(command) = command_of(level, command_node, &mut bodies) else {
            continue;
        };
        for (language, mapped) in command_scripts(level, &command) {
            if js_trim(&mapped.text()).is_empty() {
                continue;
            }
            let starts: Vec<usize> = mapped
                .starts
                .iter()
                .map(|&start| level.spans[start].0)
                .collect();
            let ends: Vec<usize> = mapped
                .ends
                .iter()
                .map(|&end| level.spans[end - 1].1)
                .collect();
            let text = mapped.text();
            let nested = language == "shellscript";
            let embedded = EmbeddedScript {
                language,
                text: text.clone(),
                starts,
                ends,
            };
            let nested_level = Level::new(
                embedded.text.clone(),
                embedded
                    .starts
                    .iter()
                    .zip(&embedded.ends)
                    .map(|(&start, &end)| (start, end))
                    .collect(),
            );
            found.push(embedded);
            if nested && depth + 1 < MAX_DEPTH {
                found.extend(scripts_in(&nested_level, depth + 1));
            }
        }
    }
    found
}

/// Builds a `Command` from a `command` node. `None` for an assignment-only command.
fn command_of<'t>(
    level: &Level,
    node: Node<'t>,
    bodies: &mut std::slice::Iter<'_, Node<'t>>,
) -> Option<Command<'t>> {
    let name_node = node.child_by_field_name("name")?;
    let name = Arg {
        node: name_node.named_child(0).unwrap_or(name_node),
    };
    let mut cursor = node.walk();
    let args = node
        .children_by_field_name("argument", &mut cursor)
        .map(|node| Arg { node })
        .collect();
    let redirects = redirects_of(node, level, bodies);
    Some(Command {
        name,
        args,
        redirects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each script's language, its text, and the part of the command that spells it.
    fn scripts_of(command: &str) -> Vec<(String, String, String)> {
        embedded_scripts(command)
            .into_iter()
            .map(|script| {
                // Every character maps inside the command, in order.
                for (index, start) in script.starts.iter().enumerate() {
                    assert!(*start < script.ends[index]);
                    if index > 0 {
                        assert!(*start >= script.ends[index - 1]);
                    }
                }
                let first = script.starts[0];
                let last = *script.ends.last().expect("a script has characters");
                (
                    script.language,
                    script.text,
                    command[first..last].to_owned(),
                )
            })
            .collect()
    }

    fn triple(language: &str, text: &str, spelled: &str) -> (String, String, String) {
        (language.to_owned(), text.to_owned(), spelled.to_owned())
    }

    const PYTHON_SCRIPT: &str = "from pathlib import Path
import json

# print every package.json name and version
for p in sorted(Path('.').glob('**/package.json')):
    data = json.loads(p.read_text())
    print(f\"{data['name']:16} {data.get('version', '?')}\")
";

    #[test]
    fn finds_the_script_inside_a_login_shell_wrapper_and_the_script_inside_that() {
        // Recorded in the turn_interrupt_mid_tool Codex transcript
        // (apps/server/src/orchestration-v2/testkit/fixtures).
        let inner = "console.log('interrupt fixture tool started'); setTimeout(() => {}, 30000)";
        let command = format!("/bin/bash -lc \"node -e \\\"{inner}\\\"\"");
        assert_eq!(
            scripts_of(&command),
            vec![
                triple(
                    "shellscript",
                    &format!("node -e \"{inner}\""),
                    &format!("node -e \\\"{inner}\\\""),
                ),
                triple("javascript", inner, inner),
            ]
        );
    }

    #[test]
    fn highlights_a_heredoc_fed_to_an_interpreter_in_that_interpreters_language() {
        let command = format!("python3 - <<'PY'\n{PYTHON_SCRIPT}PY");
        assert_eq!(
            scripts_of(&command),
            vec![triple("python", PYTHON_SCRIPT, PYTHON_SCRIPT)]
        );
    }

    #[test]
    fn maps_escaped_characters_back_to_both_source_characters() {
        let command = "/bin/zsh -lc \"python3 - <<'PY'\nprint(\\\"\\$HOME\\\")\nPY\"";
        let scripts = scripts_of(command);
        let (shell, python) = (&scripts[0], &scripts[1]);
        assert_eq!(shell.1, "python3 - <<'PY'\nprint(\"$HOME\")\nPY");
        assert_eq!(
            *python,
            triple("python", "print(\"$HOME\")\n", "print(\\\"\\$HOME\\\")\n")
        );
    }

    #[test]
    fn finds_the_inline_script_in_each_interpreter_spelling() {
        let cases: &[(&str, &str, &str)] = &[
            ("python3 -c 'print(1)'", "python", "print(1)"),
            ("python3 -X dev -c 'print(1)'", "python", "print(1)"),
            ("py -3 -c 'print(1)'", "python", "print(1)"),
            (
                "C:\\Python312\\python.exe -c 'print(1)'",
                "python",
                "print(1)",
            ),
            (
                "\"C:\\Program Files\\Python312\\python.exe\" -c 'print(1)'",
                "python",
                "print(1)",
            ),
            (
                "node --eval \"process.exit(0)\"",
                "javascript",
                "process.exit(0)",
            ),
            (
                "bun -e 'console.log(Bun.version)'",
                "typescript",
                "console.log(Bun.version)",
            ),
            ("deno eval 'console.log(1)'", "typescript", "console.log(1)"),
            (
                "deno eval --config deno.json 'console.log(1)'",
                "typescript",
                "console.log(1)",
            ),
            (
                "deno eval --env-file 'console.log(1)'",
                "typescript",
                "console.log(1)",
            ),
            ("psql -f schema.sql -c 'select 1'", "sql", "select 1"),
            (
                "node -C development -e 'console.log(1)'",
                "javascript",
                "console.log(1)",
            ),
            ("ruby -e 'puts 1'", "ruby", "puts 1"),
            ("ruby -I lib -e 'puts 1'", "ruby", "puts 1"),
            ("perl -E 'say 1'", "perl", "say 1"),
            (
                "sqlite3 app.db \"select * from users\"",
                "sql",
                "select * from users",
            ),
            ("psql -d app -c 'select 1'", "sql", "select 1"),
            (
                "pwsh -NoProfile -Command \"Get-ChildItem\"",
                "powershell",
                "Get-ChildItem",
            ),
            (
                "env FOO=1 sudo -u bob python3 -c 'print(1)'",
                "python",
                "print(1)",
            ),
            ("env -- FOO=1 python3 -c 'print(1)'", "python", "print(1)"),
            (
                "node --eval=\"process.exit(0)\"",
                "javascript",
                "process.exit(0)",
            ),
            ("psql --command='select 1'", "sql", "select 1"),
            ("mysql -c -e 'select 1'", "sql", "select 1"),
            ("mysql -f app <<'EOF'\nselect 1;\nEOF", "sql", "select 1;\n"),
            ("psql app <<'EOF'\nselect 1;\nEOF", "sql", "select 1;\n"),
            (
                "echo \"$(node -e 'console.log(1)')\"",
                "javascript",
                "console.log(1)",
            ),
            ("python3 <<< 'print(1)'", "python", "print(1)"),
        ];
        for (command, language, script) in cases {
            let found = scripts_of(command);
            assert_eq!(found.len(), 1, "{command}");
            assert_eq!(found[0].0, *language, "{command}");
            assert_eq!(found[0].1, *script, "{command}");
        }
    }

    #[test]
    fn finds_every_statement_a_sql_shell_runs() {
        for command in [
            "psql -c 'select 1' -c 'select 2'",
            "sqlite3 app.db \"select 1\" \"select 2\"",
        ] {
            let texts: Vec<(String, String)> = scripts_of(command)
                .into_iter()
                .map(|(language, text, _)| (language, text))
                .collect();
            assert_eq!(
                texts,
                vec![
                    ("sql".to_owned(), "select 1".to_owned()),
                    ("sql".to_owned(), "select 2".to_owned()),
                ],
                "{command}"
            );
        }
    }

    #[test]
    fn takes_a_heredocs_language_from_the_file_it_writes_the_shell_reading_it_or_its_delimiter() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "the file `cat` writes",
                "cat > src/app.ts <<'EOF'\nconst a = 1;\nEOF",
                "typescript",
            ),
            (
                "the file `tee` writes",
                "tee -a config.json <<EOF\n{}\nEOF",
                "json",
            ),
            (
                "its delimiter",
                "kubectl apply -f - <<'YAML'\nkind: Pod\nYAML",
                "yaml",
            ),
            (
                "the shell reading it",
                "bash <<'EOF'\necho hi\nEOF",
                "shellscript",
            ),
        ];
        for (case, command, language) in cases {
            let languages: Vec<String> = scripts_of(command)
                .into_iter()
                .map(|(lang, _, _)| lang)
                .collect();
            assert_eq!(languages, vec![(*language).to_owned()], "{case}");
        }
    }

    #[test]
    fn finds_nothing_for_script_files_modules_plain_commands_and_empty_scripts() {
        let cases: &[(&str, &str)] = &[
            ("a script file", "python3 build.py <<EOF\ndata\nEOF"),
            ("a module", "python3 -m json.tool <<EOF\n{}\nEOF"),
            ("a SQL file", "psql -f migrate.sql <<EOF\ny\nEOF"),
            (
                "a SQL file given with =",
                "psql --file=migrate.sql <<EOF\ny\nEOF",
            ),
            ("plain commands", "rg -n \"foo\" src && git status"),
            (
                "an unnamed heredoc into a plain command",
                "git commit -F - <<'EOF'\nfix: x\nEOF",
            ),
            ("an empty script", "bash -lc '   '"),
            (
                "a heredoc `cat` sends to stdout while stderr goes to a file",
                "cat 2>log.ts <<'EOF'\nx\nEOF",
            ),
        ];
        for (case, command) in cases {
            assert!(scripts_of(command).is_empty(), "{case}");
        }
    }

    #[test]
    fn trusts_nothing_in_a_command_whose_quoting_does_not_parse() {
        // Codex's secret redaction dropped the backslash of an escaped quote, so the -lc
        // argument ends early and the rest no longer balances.
        let command =
            "/bin/zsh -lc \"python3 -c 'token=[REDACTED_SECRET]\"apiToken\\\"]; print(token)'\"";
        assert!(scripts_of(command).is_empty());
    }
}
