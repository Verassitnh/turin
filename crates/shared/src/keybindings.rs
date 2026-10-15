//! Port of upstream/packages/shared/src/keybindings.ts
//!
//! Shortcut parsing, `when` expression parsing, event matching, and merging the user's keybinding
//! rules with the defaults. Upstream has no test file for this module; the tests here cover the
//! behavior the rest of the app depends on.
//!
//! Parsed shortcuts hold their key as a [`ShortcutKey`], unvalidated as upstream's parsed object is.
//! Contracts' `KeybindingValue` trims and needs 1..=64 chars, so it cannot hold the `space` key
//! (`" "`). Resolved rules carry this module's [`ResolvedKeybindingRule`] for the same reason.

use std::collections::HashSet;
use std::sync::LazyLock;

use turin_contracts::base_schemas::js_trim;
use turin_contracts::keybindings::{
    KeybindingCommand, KeybindingRule, KeybindingValue, KeybindingWhen, KeybindingWhenNode,
    MAX_KEYBINDINGS_COUNT, MAX_WHEN_EXPRESSION_DEPTH, MODEL_PICKER_JUMP_KEYBINDING_COMMANDS,
    THREAD_JUMP_KEYBINDING_COMMANDS,
};

use crate::js_string::is_js_whitespace_char;

/// The key of a parsed shortcut. Holds any non-empty text, including `" "` for `space`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShortcutKey(String);

impl ShortcutKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `ParsedShortcut` as parsed from a rule. Differs from the contracts type only in its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedShortcut {
    pub key: ShortcutKey,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub mod_key: bool,
}

/// `ResolvedKeybindingRule` with a [`ParsedShortcut`]. Differs from the contracts type only in
/// its shortcut key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedKeybindingRule {
    pub command: KeybindingCommand,
    pub shortcut: ParsedShortcut,
    pub when_ast: Option<KeybindingWhenNode>,
}

/// The fields of a keyboard event that shortcut matching reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutEvent {
    pub key: String,
    pub code: Option<String>,
    /// `getModifierState("AltGraph")`. False when the event cannot report it.
    pub alt_graph: bool,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
}

impl ShortcutEvent {
    fn modifiers(&self) -> ModifierState {
        ModifierState {
            meta_key: self.meta_key,
            ctrl_key: self.ctrl_key,
            shift_key: self.shift_key,
            alt_key: self.alt_key,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierState {
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
}

/// `EVENT_CODE_SHORTCUT_KEYS`: physical keys whose shortcut character differs by layout.
fn event_code_shortcut_key(code: &str) -> Option<&'static str> {
    Some(match code {
        "Backquote" => "`",
        "Backslash" => "\\",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Comma" => ",",
        "Digit0" => "0",
        "Digit1" => "1",
        "Digit2" => "2",
        "Digit3" => "3",
        "Digit4" => "4",
        "Digit5" => "5",
        "Digit6" => "6",
        "Digit7" => "7",
        "Digit8" => "8",
        "Digit9" => "9",
        "Equal" => "=",
        "Minus" => "-",
        "Period" => ".",
        "Quote" => "'",
        "Semicolon" => ";",
        "Slash" => "/",
        _ => return None,
    })
}

/// `/mac|iphone|ipad|ipod/i`. ASCII case folding, as a non-Unicode JS regex does.
pub fn is_mac_platform(platform: &str) -> bool {
    let platform = platform.to_ascii_lowercase();
    ["mac", "iphone", "ipad", "ipod"]
        .iter()
        .any(|needle| platform.contains(needle))
}

pub fn normalize_event_key(key: &str) -> String {
    let normalized = key.to_lowercase();
    if normalized == "esc" {
        return "escape".to_owned();
    }
    normalized
}

/// `/^[a-z]$/`
fn is_single_ascii_lower(value: &str) -> bool {
    let mut chars = value.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_lowercase())
}

/// `/^[a-z0-9]$/i`
fn is_single_ascii_alphanumeric(value: &str) -> bool {
    let mut chars = value.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphanumeric())
}

pub fn shortcut_key_from_event(key: &str, code: Option<&str>) -> String {
    let layout_key = normalize_event_key(key);
    if is_single_ascii_lower(&layout_key) {
        return layout_key;
    }
    code.and_then(event_code_shortcut_key)
        .map(str::to_owned)
        .unwrap_or(layout_key)
}

/// `/^Key([A-Z])$/` on `event.code`, lowercased.
fn letter_code(code: &str) -> Option<char> {
    let letter = code.strip_prefix("Key")?;
    let mut chars = letter.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// Every key a physical press can answer to: the layout key, the letter of its physical key, and
/// the shortcut key from [`shortcut_key_from_event`].
pub fn resolve_event_keys(event: &ShortcutEvent) -> HashSet<String> {
    let layout_key = normalize_event_key(&event.key);
    let mut keys = HashSet::from([layout_key.clone()]);
    if let Some(letter) = event.code.as_deref().and_then(letter_code)
        && !is_single_ascii_lower(&layout_key)
    {
        keys.insert(letter.to_string());
    }
    keys.insert(shortcut_key_from_event(&event.key, event.code.as_deref()));
    keys
}

pub fn matches_keybinding_shortcut_modifiers(
    event: &ModifierState,
    shortcut: &ParsedShortcut,
    platform: &str,
) -> bool {
    let use_meta_for_mod = is_mac_platform(platform);
    let expected_meta = shortcut.meta_key || (shortcut.mod_key && use_meta_for_mod);
    let expected_ctrl = shortcut.ctrl_key || (shortcut.mod_key && !use_meta_for_mod);
    event.meta_key == expected_meta
        && event.ctrl_key == expected_ctrl
        && event.shift_key == shortcut.shift_key
        && event.alt_key == shortcut.alt_key
}

pub fn matches_keybinding_shortcut(
    event: &ShortcutEvent,
    shortcut: &ParsedShortcut,
    platform: &str,
) -> bool {
    // AltGraph layouts type characters through Alt+Ctrl; only plain alphanumerics keep matching.
    if !is_mac_platform(platform) && event.alt_graph && !is_single_ascii_alphanumeric(&event.key) {
        return false;
    }
    if !matches_keybinding_shortcut_modifiers(&event.modifiers(), shortcut, platform) {
        return false;
    }
    resolve_event_keys(event).contains(shortcut.key.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WhenToken {
    Identifier(String),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

pub fn normalize_key_token(token: &str) -> &str {
    match token {
        "space" => " ",
        "esc" => "escape",
        other => other,
    }
}

/// `value` is one shortcut such as `mod+shift+b`. Returns `None` when a token is empty, a key
/// appears twice, or no key is present.
pub fn parse_keybinding_shortcut(value: &str) -> Option<ParsedShortcut> {
    let mut tokens: Vec<String> = value
        .to_lowercase()
        .split('+')
        .map(|token| js_trim(token).to_owned())
        .collect();
    // A trailing `+` is the key itself (`mod++`), so the empty tokens it leaves are replaced with it.
    let mut trailing_empty_count = 0usize;
    while tokens.last().is_some_and(String::is_empty) {
        trailing_empty_count += 1;
        tokens.pop();
    }
    if trailing_empty_count > 0 {
        tokens.push("+".to_owned());
    }
    if tokens.iter().any(String::is_empty) || tokens.is_empty() {
        return None;
    }

    let mut key: Option<String> = None;
    let (mut meta_key, mut ctrl_key, mut shift_key, mut alt_key, mut mod_key) =
        (false, false, false, false, false);
    for token in &tokens {
        match token.as_str() {
            "cmd" | "meta" => meta_key = true,
            "ctrl" | "control" => ctrl_key = true,
            "shift" => shift_key = true,
            "alt" | "option" => alt_key = true,
            "mod" => mod_key = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(normalize_key_token(other).to_owned());
            }
        }
    }

    Some(ParsedShortcut {
        key: ShortcutKey(key?),
        meta_key,
        ctrl_key,
        shift_key,
        alt_key,
        mod_key,
    })
}

/// `^[A-Za-z_][A-Za-z0-9_.-]*` at the start of `text`, as a byte length.
fn identifier_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    Some(
        bytes
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(**b, b'_' | b'.' | b'-'))
            .count(),
    )
}

fn tokenize_when_expression(expression: &str) -> Option<Vec<WhenToken>> {
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < expression.len() {
        let rest = &expression[index..];
        let current = rest.chars().next()?;
        if is_js_whitespace_char(current) {
            index += current.len_utf8();
            continue;
        }
        if rest.starts_with("&&") {
            tokens.push(WhenToken::And);
            index += 2;
            continue;
        }
        if rest.starts_with("||") {
            tokens.push(WhenToken::Or);
            index += 2;
            continue;
        }
        let punctuation = match current {
            '!' => Some(WhenToken::Not),
            '(' => Some(WhenToken::LParen),
            ')' => Some(WhenToken::RParen),
            _ => None,
        };
        if let Some(token) = punctuation {
            tokens.push(token);
            index += 1;
            continue;
        }
        let len = identifier_len(rest)?;
        tokens.push(WhenToken::Identifier(rest[..len].to_owned()));
        index += len;
    }
    Some(tokens)
}

/// Recursive descent over the tokens. Each parse method returns `None` on a syntax error, and the
/// depth limit bounds nesting and `!` chains.
struct WhenParser {
    tokens: Vec<WhenToken>,
    index: usize,
}

impl WhenParser {
    fn parse_primary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        if depth > MAX_WHEN_EXPRESSION_DEPTH {
            return None;
        }
        let token = self.tokens.get(self.index)?.clone();
        match token {
            WhenToken::Identifier(name) => {
                self.index += 1;
                Some(KeybindingWhenNode::Identifier { name })
            }
            WhenToken::LParen => {
                self.index += 1;
                let node = self.parse_or(depth + 1)?;
                if self.tokens.get(self.index) != Some(&WhenToken::RParen) {
                    return None;
                }
                self.index += 1;
                Some(node)
            }
            _ => None,
        }
    }

    fn parse_unary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut not_count = 0usize;
        while self.tokens.get(self.index) == Some(&WhenToken::Not) {
            self.index += 1;
            not_count += 1;
            if not_count > MAX_WHEN_EXPRESSION_DEPTH {
                return None;
            }
        }
        let mut node = self.parse_primary(depth)?;
        for _ in 0..not_count {
            node = KeybindingWhenNode::Not {
                node: Box::new(node),
            };
        }
        Some(node)
    }

    fn parse_and(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut left = self.parse_unary(depth)?;
        while self.tokens.get(self.index) == Some(&WhenToken::And) {
            self.index += 1;
            let right = self.parse_unary(depth)?;
            left = KeybindingWhenNode::And {
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Some(left)
    }

    fn parse_or(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut left = self.parse_and(depth)?;
        while self.tokens.get(self.index) == Some(&WhenToken::Or) {
            self.index += 1;
            let right = self.parse_and(depth)?;
            left = KeybindingWhenNode::Or {
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Some(left)
    }
}

pub fn parse_keybinding_when_expression(expression: &str) -> Option<KeybindingWhenNode> {
    let tokens = tokenize_when_expression(expression)?;
    if tokens.is_empty() {
        return None;
    }
    let token_count = tokens.len();
    let mut parser = WhenParser { tokens, index: 0 };
    let ast = parser.parse_or(0)?;
    if parser.index != token_count {
        return None;
    }
    Some(ast)
}

pub fn compile_resolved_keybinding_rule(rule: &KeybindingRule) -> Option<ResolvedKeybindingRule> {
    let shortcut = parse_keybinding_shortcut(rule.key.as_str())?;
    let when_ast = match &rule.when {
        Some(when) => Some(parse_keybinding_when_expression(when.as_str())?),
        None => None,
    };
    Some(ResolvedKeybindingRule {
        command: rule.command.clone(),
        shortcut,
        when_ast,
    })
}

/// Rules that do not compile are skipped. Only the last [`MAX_KEYBINDINGS_COUNT`] are kept.
pub fn compile_resolved_keybindings_config(
    config: &[KeybindingRule],
) -> Vec<ResolvedKeybindingRule> {
    let compiled: Vec<ResolvedKeybindingRule> = config
        .iter()
        .filter_map(compile_resolved_keybinding_rule)
        .collect();
    let start = compiled.len().saturating_sub(MAX_KEYBINDINGS_COUNT);
    compiled[start..].to_vec()
}

fn default_rule(key: &str, command: &str, when: Option<&str>) -> KeybindingRule {
    KeybindingRule {
        key: KeybindingValue::new(key).expect("default keybinding key"),
        command: KeybindingCommand::new(command).expect("default keybinding command"),
        when: when.map(|when| KeybindingWhen::new(when).expect("default keybinding when")),
    }
}

/// `DEFAULT_KEYBINDINGS`. Order matters: later rules win when a shortcut is shared.
pub fn default_keybinding_rules() -> Vec<KeybindingRule> {
    let mut rules = vec![
        default_rule("mod+b", "sidebar.toggle", None),
        default_rule("mod+[", "navigation.back", Some("!terminalFocus")),
        default_rule("mod+]", "navigation.forward", Some("!terminalFocus")),
        default_rule("mod+j", "terminal.toggle", None),
        default_rule("mod+alt+b", "rightPanel.toggle", None),
        default_rule("mod+shift+b", "threadPanel.toggle", Some("!terminalFocus")),
        default_rule(
            "mod+t",
            "rightPanel.new",
            Some("rightPanelOpen && !terminalFocus"),
        ),
        default_rule("mod+d", "terminal.split", Some("terminalFocus")),
        default_rule(
            "mod+shift+d",
            "terminal.splitVertical",
            Some("terminalFocus"),
        ),
        default_rule("mod+n", "terminal.new", Some("terminalFocus")),
        default_rule("mod+w", "terminal.close", Some("terminalFocus")),
        default_rule("mod+w", "rightPanel.close", Some("!terminalFocus")),
        default_rule("mod+shift+t", "view.reopenClosed", None),
        default_rule("mod+d", "diff.toggle", Some("!terminalFocus")),
        default_rule("mod+shift+j", "preview.toggle", None),
        default_rule("mod+r", "preview.refresh", Some("previewFocus")),
        default_rule("mod+l", "preview.focusUrl", Some("previewFocus")),
        default_rule("mod+=", "preview.zoomIn", Some("previewFocus")),
        default_rule("mod++", "preview.zoomIn", Some("previewFocus")),
        default_rule("mod+-", "preview.zoomOut", Some("previewFocus")),
        default_rule("mod+0", "preview.resetZoom", Some("previewFocus")),
        default_rule("mod+k", "commandPalette.toggle", Some("!terminalFocus")),
        default_rule("mod+p", "filePicker.toggle", Some("!terminalFocus")),
        default_rule(
            "mod+shift+f",
            "projectSearch.toggle",
            Some("!terminalFocus"),
        ),
        default_rule("mod+u", "usage.open", Some("!terminalFocus")),
        default_rule("mod+alt+a", "theme.select", Some("!terminalFocus")),
        default_rule(
            "mod+alt+shift+a",
            "appearance.cycle",
            Some("!terminalFocus"),
        ),
        default_rule("mod+alt+shift+t", "themeEditor.toggle", None),
        default_rule("mod+s", "composer.stash", Some("!terminalFocus")),
        default_rule(
            "mod+shift+enter",
            "thread.steerQueuedMessage",
            Some("!terminalFocus"),
        ),
        default_rule(
            "alt+arrowup",
            "thread.editQueuedMessage",
            Some("composerFocus"),
        ),
        default_rule(
            "mod+enter",
            "composer.sendAlternate",
            Some("composerFocus && turnRunning"),
        ),
        default_rule(
            "mod+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
        default_rule(
            "mod+alt+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
        default_rule(
            "mod+alt+enter",
            "composer.sendAndNewThread",
            Some("composerFocus && !draftThreadRoute"),
        ),
        default_rule("mod+n", "chat.new", Some("!terminalFocus")),
        default_rule("mod+shift+o", "chat.new", Some("!terminalFocus")),
        default_rule("mod+shift+n", "chat.newLocal", Some("!terminalFocus")),
        default_rule(
            "mod+alt+n",
            "chat.newWithoutProject",
            Some("!terminalFocus"),
        ),
        default_rule(
            "mod+f",
            "chat.find",
            Some("!terminalFocus && !previewFocus"),
        ),
        default_rule("mod+shift+m", "modelPicker.toggle", Some("!terminalFocus")),
        default_rule("mod+shift+h", "composer.host", Some("!terminalFocus")),
        default_rule("mod+shift+e", "composer.effort", Some("!terminalFocus")),
        default_rule("mod+shift+a", "composer.mode", Some("!terminalFocus")),
        default_rule("mod+shift+x", "composer.workspace", Some("!terminalFocus")),
        default_rule("mod+shift+g", "composer.branch", Some("!terminalFocus")),
        default_rule(
            "mod+shift+l",
            "composer.previousWorktree",
            Some("!terminalFocus"),
        ),
        default_rule(
            "mod+shift+k",
            "pullRequest.copyNumber",
            Some("!terminalFocus"),
        ),
        default_rule(
            "mod+shift+arrowup",
            "modelPicker.previousProvider",
            Some("modelPickerOpen"),
        ),
        default_rule(
            "mod+shift+arrowdown",
            "modelPicker.nextProvider",
            Some("modelPickerOpen"),
        ),
        default_rule("mod+o", "editor.openFavorite", None),
        default_rule("mod+shift+[", "thread.previous", None),
        default_rule("mod+shift+]", "thread.next", None),
        default_rule(
            "mod+shift+c",
            "thread.copyReference",
            Some("!terminalFocus"),
        ),
        default_rule("mod+shift+s", "thread.settle", Some("!terminalFocus")),
        default_rule("mod+shift+p", "thread.pin", Some("!terminalFocus")),
        default_rule(
            "mod+z",
            "thread.undo",
            Some("!terminalFocus && !editableFocus"),
        ),
    ];
    for (index, command) in THREAD_JUMP_KEYBINDING_COMMANDS.iter().enumerate() {
        rules.push(default_rule(
            &format!("mod+{}", index + 1),
            command,
            Some("!modelPickerOpen && isDesktop"),
        ));
    }
    for (index, command) in MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.iter().enumerate() {
        rules.push(default_rule(
            &format!("mod+{}", index + 1),
            command,
            Some("modelPickerOpen && isDesktop"),
        ));
    }
    rules.extend([
        default_rule("c", "usage.cost", Some("usagePageOpen")),
        default_rule("t", "usage.tokens", Some("usagePageOpen")),
        default_rule("l", "usage.limits", Some("usagePageOpen")),
        default_rule("mod+shift+1", "usage.period.day", Some("usagePageOpen")),
        default_rule("mod+shift+2", "usage.period.week", Some("usagePageOpen")),
        default_rule("mod+shift+3", "usage.period.month", Some("usagePageOpen")),
        default_rule("mod+shift+4", "usage.period.quarter", Some("usagePageOpen")),
    ]);
    rules
}

pub static DEFAULT_RESOLVED_KEYBINDINGS: LazyLock<Vec<ResolvedKeybindingRule>> =
    LazyLock::new(|| compile_resolved_keybindings_config(&default_keybinding_rules()));

/// `mergeWithDefaultKeybindings`: a user rule replaces every default bound to the same command.
pub fn merge_with_default_keybindings(
    custom: &[ResolvedKeybindingRule],
) -> Vec<ResolvedKeybindingRule> {
    if custom.is_empty() {
        return DEFAULT_RESOLVED_KEYBINDINGS.clone();
    }
    let overridden: HashSet<&str> = custom
        .iter()
        .map(|binding| binding.command.as_str())
        .collect();
    let merged: Vec<ResolvedKeybindingRule> = DEFAULT_RESOLVED_KEYBINDINGS
        .iter()
        .filter(|binding| !overridden.contains(binding.command.as_str()))
        .cloned()
        .chain(custom.iter().cloned())
        .collect();
    if merged.len() <= MAX_KEYBINDINGS_COUNT {
        return merged;
    }
    merged[merged.len() - MAX_KEYBINDINGS_COUNT..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::keybindings::KeybindingWhenNode as N;

    fn shortcut(value: &str) -> ParsedShortcut {
        parse_keybinding_shortcut(value).unwrap_or_else(|| panic!("{value} parses"))
    }

    fn event(key: &str, code: Option<&str>) -> ShortcutEvent {
        ShortcutEvent {
            key: key.to_owned(),
            code: code.map(str::to_owned),
            ..ShortcutEvent::default()
        }
    }

    fn ident(name: &str) -> N {
        N::Identifier {
            name: name.to_owned(),
        }
    }

    #[test]
    fn parses_modifiers_and_keys() {
        let parsed = shortcut("MOD+Shift+B");
        assert_eq!(parsed.key.as_str(), "b");
        assert!(parsed.mod_key && parsed.shift_key);
        assert!(!parsed.meta_key && !parsed.ctrl_key && !parsed.alt_key);
        assert_eq!(shortcut("esc").key.as_str(), "escape");
        assert_eq!(shortcut("ctrl+option+cmd+x").key.as_str(), "x");
    }

    #[test]
    fn a_trailing_plus_is_the_key() {
        assert_eq!(shortcut("mod++").key.as_str(), "+");
        assert_eq!(shortcut("+").key.as_str(), "+");
    }

    #[test]
    fn rejects_empty_tokens_repeated_keys_and_missing_keys() {
        for bad in ["mod+shift", "mod+a+b", "mod", "mod+ +x"] {
            assert_eq!(parse_keybinding_shortcut(bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_the_space_key_and_matches_a_space_press() {
        let spec = shortcut("mod+space");
        assert_eq!(spec.key.as_str(), " ");
        let press = ShortcutEvent {
            meta_key: true,
            ..event(" ", Some("Space"))
        };
        assert!(matches_keybinding_shortcut(&press, &spec, "MacIntel"));
        assert!(!matches_keybinding_shortcut(
            &event("x", None),
            &spec,
            "MacIntel"
        ));
    }

    #[test]
    fn keys_longer_than_contract_values_are_kept_as_upstream_keeps_them() {
        let long = "x".repeat(70);
        assert_eq!(shortcut(&format!("mod+{long}")).key.as_str(), long);
    }

    #[test]
    fn parses_when_expressions_with_precedence() {
        let ast = parse_keybinding_when_expression("!a && b || (c)").unwrap();
        assert_eq!(
            ast,
            N::Or {
                left: Box::new(N::And {
                    left: Box::new(N::Not {
                        node: Box::new(ident("a")),
                    }),
                    right: Box::new(ident("b")),
                }),
                right: Box::new(ident("c")),
            }
        );
    }

    #[test]
    fn rejects_malformed_when_expressions() {
        for bad in ["", "a &&", "(a", "a)", "a b", "a & b", "é", "!", "()"] {
            assert_eq!(parse_keybinding_when_expression(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bounds_when_expression_depth_and_negation_chains() {
        let nots = "!".repeat(MAX_WHEN_EXPRESSION_DEPTH);
        assert!(parse_keybinding_when_expression(&format!("{nots}a")).is_some());
        let too_many = "!".repeat(MAX_WHEN_EXPRESSION_DEPTH + 1);
        assert_eq!(
            parse_keybinding_when_expression(&format!("{too_many}a")),
            None
        );
        let parens = format!(
            "{}a{}",
            "(".repeat(MAX_WHEN_EXPRESSION_DEPTH + 1),
            ")".repeat(MAX_WHEN_EXPRESSION_DEPTH + 1)
        );
        assert_eq!(parse_keybinding_when_expression(&parens), None);
    }

    #[test]
    fn matches_mod_as_meta_on_mac_and_ctrl_elsewhere() {
        let spec = shortcut("mod+b");
        let mac_event = ShortcutEvent {
            meta_key: true,
            ..event("b", Some("KeyB"))
        };
        let linux_event = ShortcutEvent {
            ctrl_key: true,
            ..event("b", Some("KeyB"))
        };
        assert!(matches_keybinding_shortcut(&mac_event, &spec, "MacIntel"));
        assert!(!matches_keybinding_shortcut(
            &linux_event,
            &spec,
            "MacIntel"
        ));
        assert!(matches_keybinding_shortcut(
            &linux_event,
            &spec,
            "Linux x86_64"
        ));
        assert!(!matches_keybinding_shortcut(
            &mac_event,
            &spec,
            "Linux x86_64"
        ));
    }

    #[test]
    fn non_latin_layouts_fall_back_to_the_physical_letter_or_symbol() {
        // Cyrillic `ф` sits on the physical A key: `mod+a` answers to it.
        let cyrillic = ShortcutEvent {
            meta_key: true,
            ..event("ф", Some("KeyA"))
        };
        assert!(matches_keybinding_shortcut(
            &cyrillic,
            &shortcut("mod+a"),
            "MacIntel"
        ));
        // An ASCII layout letter is authoritative: `c` on a Dvorak `KeyI` is still `c`.
        let dvorak = ShortcutEvent {
            meta_key: true,
            ..event("c", Some("KeyI"))
        };
        assert!(!matches_keybinding_shortcut(
            &dvorak,
            &shortcut("mod+i"),
            "MacIntel"
        ));
        assert!(matches_keybinding_shortcut(
            &dvorak,
            &shortcut("mod+c"),
            "MacIntel"
        ));
        // A symbol shortcut uses the physical code when the layout key is not a letter.
        let bracket = ShortcutEvent {
            meta_key: true,
            ..event("å", Some("BracketLeft"))
        };
        assert!(matches_keybinding_shortcut(
            &bracket,
            &shortcut("mod+["),
            "MacIntel"
        ));
    }

    #[test]
    fn altgraph_blocks_non_alphanumeric_matches_off_mac() {
        let spec = shortcut("ctrl+@");
        let altgraph = ShortcutEvent {
            alt_graph: true,
            ctrl_key: true,
            ..event("@", None)
        };
        assert!(!matches_keybinding_shortcut(&altgraph, &spec, "Linux"));
        assert!(matches_keybinding_shortcut(&altgraph, &spec, "MacIntel"));
    }

    #[test]
    fn default_rules_all_compile() {
        // 57 fixed rules, then the thread jumps, the model picker jumps, and 7 usage rules.
        let defaults = default_keybinding_rules();
        assert_eq!(
            defaults.len(),
            57 + THREAD_JUMP_KEYBINDING_COMMANDS.len()
                + MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.len()
                + 7
        );
        assert_eq!(DEFAULT_RESOLVED_KEYBINDINGS.len(), defaults.len());
    }

    #[test]
    fn merge_replaces_defaults_bound_to_the_same_command() {
        let custom = compile_resolved_keybindings_config(&[default_rule(
            "mod+shift+x",
            "sidebar.toggle",
            None,
        )]);
        let merged = merge_with_default_keybindings(&custom);
        let sidebar: Vec<&str> = merged
            .iter()
            .filter(|binding| binding.command.as_str() == "sidebar.toggle")
            .map(|binding| binding.shortcut.key.as_str())
            .collect();
        assert_eq!(sidebar, ["x"]);
        assert_eq!(merged.len(), DEFAULT_RESOLVED_KEYBINDINGS.len());
        assert_eq!(
            merge_with_default_keybindings(&[]).len(),
            DEFAULT_RESOLVED_KEYBINDINGS.len()
        );
    }
}
