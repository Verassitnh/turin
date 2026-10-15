//! Port of upstream/packages/shared/src/keybindings.ts, server subset.
//!
//! Shortcut parsing, `when` expression parsing, rule compilation and the
//! default-merge rules. The browser event matchers (`matchesKeybindingShortcut`
//! and friends) belong to the UI port and are not ported here.

use std::sync::LazyLock;

use turin_contracts::base_schemas::js_trim;
use turin_contracts::keybindings::{
    KeybindingCommand, KeybindingRule, KeybindingShortcut, KeybindingValue, KeybindingWhen,
    KeybindingWhenNode, MAX_KEYBINDINGS_COUNT, MAX_WHEN_EXPRESSION_DEPTH,
    MODEL_PICKER_JUMP_KEYBINDING_COMMANDS, ResolvedKeybindingRule, THREAD_JUMP_KEYBINDING_COMMANDS,
};

fn rule(key: &str, command: &str, when: Option<&str>) -> KeybindingRule {
    KeybindingRule {
        key: KeybindingValue::new(key).expect("static default keybinding key"),
        command: KeybindingCommand::new(command).expect("static default keybinding command"),
        when: when.map(|when| KeybindingWhen::new(when).expect("static default when")),
    }
}

/// `DEFAULT_KEYBINDINGS`, in upstream order. Order matters: later rules win
/// in `mergeWithDefaultKeybindings`, and the startup backfill appends in this
/// order too.
pub static DEFAULT_KEYBINDINGS: LazyLock<Vec<KeybindingRule>> = LazyLock::new(|| {
    let mut rules = vec![
        rule("mod+b", "sidebar.toggle", None),
        rule("mod+[", "navigation.back", Some("!terminalFocus")),
        rule("mod+]", "navigation.forward", Some("!terminalFocus")),
        rule("mod+j", "terminal.toggle", None),
        rule("mod+alt+b", "rightPanel.toggle", None),
        rule("mod+shift+b", "threadPanel.toggle", Some("!terminalFocus")),
        rule(
            "mod+t",
            "rightPanel.new",
            Some("rightPanelOpen && !terminalFocus"),
        ),
        rule("mod+d", "terminal.split", Some("terminalFocus")),
        rule(
            "mod+shift+d",
            "terminal.splitVertical",
            Some("terminalFocus"),
        ),
        rule("mod+n", "terminal.new", Some("terminalFocus")),
        rule("mod+w", "terminal.close", Some("terminalFocus")),
        rule("mod+w", "rightPanel.close", Some("!terminalFocus")),
        rule("mod+shift+t", "view.reopenClosed", None),
        rule("mod+d", "diff.toggle", Some("!terminalFocus")),
        rule("mod+shift+j", "preview.toggle", None),
        rule("mod+r", "preview.refresh", Some("previewFocus")),
        rule("mod+l", "preview.focusUrl", Some("previewFocus")),
        rule("mod+=", "preview.zoomIn", Some("previewFocus")),
        rule("mod++", "preview.zoomIn", Some("previewFocus")),
        rule("mod+-", "preview.zoomOut", Some("previewFocus")),
        rule("mod+0", "preview.resetZoom", Some("previewFocus")),
        rule("mod+k", "commandPalette.toggle", Some("!terminalFocus")),
        rule("mod+p", "filePicker.toggle", Some("!terminalFocus")),
        rule(
            "mod+shift+f",
            "projectSearch.toggle",
            Some("!terminalFocus"),
        ),
        rule("mod+u", "usage.open", Some("!terminalFocus")),
        rule("mod+alt+a", "theme.select", Some("!terminalFocus")),
        rule(
            "mod+alt+shift+a",
            "appearance.cycle",
            Some("!terminalFocus"),
        ),
        rule("mod+alt+shift+t", "themeEditor.toggle", None),
        rule("mod+s", "composer.stash", Some("!terminalFocus")),
        rule(
            "mod+shift+enter",
            "thread.steerQueuedMessage",
            Some("!terminalFocus"),
        ),
        rule(
            "alt+arrowup",
            "thread.editQueuedMessage",
            Some("composerFocus"),
        ),
        rule(
            "mod+enter",
            "composer.sendAlternate",
            Some("composerFocus && turnRunning"),
        ),
        rule(
            "mod+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
        rule(
            "mod+alt+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
        rule(
            "mod+alt+enter",
            "composer.sendAndNewThread",
            Some("composerFocus && !draftThreadRoute"),
        ),
        rule("mod+n", "chat.new", Some("!terminalFocus")),
        rule("mod+shift+o", "chat.new", Some("!terminalFocus")),
        rule("mod+shift+n", "chat.newLocal", Some("!terminalFocus")),
        rule(
            "mod+alt+n",
            "chat.newWithoutProject",
            Some("!terminalFocus"),
        ),
        rule(
            "mod+f",
            "chat.find",
            Some("!terminalFocus && !previewFocus"),
        ),
        rule("mod+shift+m", "modelPicker.toggle", Some("!terminalFocus")),
        rule("mod+shift+h", "composer.host", Some("!terminalFocus")),
        rule("mod+shift+e", "composer.effort", Some("!terminalFocus")),
        rule("mod+shift+a", "composer.mode", Some("!terminalFocus")),
        rule("mod+shift+x", "composer.workspace", Some("!terminalFocus")),
        rule("mod+shift+g", "composer.branch", Some("!terminalFocus")),
        rule(
            "mod+shift+l",
            "composer.previousWorktree",
            Some("!terminalFocus"),
        ),
        rule(
            "mod+shift+k",
            "pullRequest.copyNumber",
            Some("!terminalFocus"),
        ),
        rule(
            "mod+shift+arrowup",
            "modelPicker.previousProvider",
            Some("modelPickerOpen"),
        ),
        rule(
            "mod+shift+arrowdown",
            "modelPicker.nextProvider",
            Some("modelPickerOpen"),
        ),
        rule("mod+o", "editor.openFavorite", None),
        rule("mod+shift+[", "thread.previous", None),
        rule("mod+shift+]", "thread.next", None),
        rule(
            "mod+shift+c",
            "thread.copyReference",
            Some("!terminalFocus"),
        ),
        rule("mod+shift+s", "thread.settle", Some("!terminalFocus")),
        rule("mod+shift+p", "thread.pin", Some("!terminalFocus")),
        rule(
            "mod+z",
            "thread.undo",
            Some("!terminalFocus && !editableFocus"),
        ),
    ];
    for (index, command) in THREAD_JUMP_KEYBINDING_COMMANDS.iter().enumerate() {
        rules.push(rule(
            &format!("mod+{}", index + 1),
            command,
            Some("!modelPickerOpen && isDesktop"),
        ));
    }
    for (index, command) in MODEL_PICKER_JUMP_KEYBINDING_COMMANDS.iter().enumerate() {
        rules.push(rule(
            &format!("mod+{}", index + 1),
            command,
            Some("modelPickerOpen && isDesktop"),
        ));
    }
    rules.extend([
        rule("c", "usage.cost", Some("usagePageOpen")),
        rule("t", "usage.tokens", Some("usagePageOpen")),
        rule("l", "usage.limits", Some("usagePageOpen")),
        rule("mod+shift+1", "usage.period.day", Some("usagePageOpen")),
        rule("mod+shift+2", "usage.period.week", Some("usagePageOpen")),
        rule("mod+shift+3", "usage.period.month", Some("usagePageOpen")),
        rule("mod+shift+4", "usage.period.quarter", Some("usagePageOpen")),
    ]);
    rules
});

fn normalize_key_token(token: &str) -> &str {
    match token {
        "space" => " ",
        "esc" => "escape",
        other => other,
    }
}

/// `parseKeybindingShortcut`. Lowercases, splits on `+`, and treats a trailing
/// `+` (e.g. `mod++`) as the literal `+` key. Returns `None` when the shortcut
/// has an empty token, two keys, or no key.
pub fn parse_keybinding_shortcut(value: &str) -> Option<KeybindingShortcut> {
    let mut tokens: Vec<String> = value
        .to_lowercase()
        .split('+')
        .map(|token| js_trim(token).to_owned())
        .collect();
    let mut trailing_empty_count = 0;
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

    let key = key?;
    Some(KeybindingShortcut {
        key: KeybindingValue::new(key).ok()?,
        meta_key,
        ctrl_key,
        shift_key,
        alt_key,
        mod_key,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WhenToken {
    Identifier(String),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

/// `tokenizeWhenExpression`. Any character outside the grammar makes the whole
/// expression invalid (`None`).
fn tokenize_when_expression(expression: &str) -> Option<Vec<WhenToken>> {
    let chars: Vec<char> = expression.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current.is_whitespace() {
            index += 1;
            continue;
        }
        if chars[index..].starts_with(&['&', '&']) {
            tokens.push(WhenToken::And);
            index += 2;
            continue;
        }
        if chars[index..].starts_with(&['|', '|']) {
            tokens.push(WhenToken::Or);
            index += 2;
            continue;
        }
        match current {
            '!' => tokens.push(WhenToken::Not),
            '(' => tokens.push(WhenToken::LParen),
            ')' => tokens.push(WhenToken::RParen),
            _ => {
                if !(current.is_ascii_alphabetic() || current == '_') {
                    return None;
                }
                let start = index;
                index += 1;
                while index < chars.len()
                    && (chars[index].is_ascii_alphanumeric()
                        || matches!(chars[index], '_' | '.' | '-'))
                {
                    index += 1;
                }
                tokens.push(WhenToken::Identifier(chars[start..index].iter().collect()));
                continue;
            }
        }
        index += 1;
    }
    Some(tokens)
}

struct WhenParser {
    tokens: Vec<WhenToken>,
    index: usize,
}

impl WhenParser {
    fn peek(&self) -> Option<&WhenToken> {
        self.tokens.get(self.index)
    }

    // Precedence: primary (identifier or parenthesised or) < unary `!` < `&&` < `||`.
    fn parse_primary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        if depth > MAX_WHEN_EXPRESSION_DEPTH {
            return None;
        }
        match self.peek()?.clone() {
            WhenToken::Identifier(name) => {
                self.index += 1;
                Some(KeybindingWhenNode::Identifier { name })
            }
            WhenToken::LParen => {
                self.index += 1;
                let node = self.parse_or(depth + 1)?;
                if self.peek() != Some(&WhenToken::RParen) {
                    return None;
                }
                self.index += 1;
                Some(node)
            }
            _ => None,
        }
    }

    fn parse_unary(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut not_count = 0;
        while self.peek() == Some(&WhenToken::Not) {
            self.index += 1;
            not_count += 1;
            if not_count > MAX_WHEN_EXPRESSION_DEPTH {
                return None;
            }
        }
        let mut node = self.parse_primary(depth)?;
        while not_count > 0 {
            node = KeybindingWhenNode::Not {
                node: Box::new(node),
            };
            not_count -= 1;
        }
        Some(node)
    }

    fn parse_and(&mut self, depth: usize) -> Option<KeybindingWhenNode> {
        let mut left = self.parse_unary(depth)?;
        while self.peek() == Some(&WhenToken::And) {
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
        while self.peek() == Some(&WhenToken::Or) {
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

/// `parseKeybindingWhenExpression`. Recursive descent; the whole input must be
/// consumed.
pub fn parse_keybinding_when_expression(expression: &str) -> Option<KeybindingWhenNode> {
    let tokens = tokenize_when_expression(expression)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = WhenParser { tokens, index: 0 };
    let ast = parser.parse_or(0)?;
    if parser.index != parser.tokens.len() {
        return None;
    }
    Some(ast)
}

/// `compileResolvedKeybindingRule`: `None` when the shortcut or the `when`
/// expression does not parse.
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

/// `compileResolvedKeybindingsConfig`: drops invalid rules, keeps the last
/// [`MAX_KEYBINDINGS_COUNT`].
pub fn compile_resolved_keybindings_config(
    config: &[KeybindingRule],
) -> Vec<ResolvedKeybindingRule> {
    let compiled: Vec<ResolvedKeybindingRule> = config
        .iter()
        .filter_map(compile_resolved_keybinding_rule)
        .collect();
    keep_last_max(compiled)
}

/// `DEFAULT_RESOLVED_KEYBINDINGS`.
pub static DEFAULT_RESOLVED_KEYBINDINGS: LazyLock<Vec<ResolvedKeybindingRule>> =
    LazyLock::new(|| compile_resolved_keybindings_config(&DEFAULT_KEYBINDINGS));

/// `mergeWithDefaultKeybindings`: a default rule is dropped when the custom
/// config rebinds its command; custom rules always come last so they win.
pub fn merge_with_default_keybindings(
    custom: Vec<ResolvedKeybindingRule>,
) -> Vec<ResolvedKeybindingRule> {
    if custom.is_empty() {
        return DEFAULT_RESOLVED_KEYBINDINGS.clone();
    }
    let overridden: std::collections::HashSet<&str> = custom
        .iter()
        .map(|binding| binding.command.as_str())
        .collect();
    let mut merged: Vec<ResolvedKeybindingRule> = DEFAULT_RESOLVED_KEYBINDINGS
        .iter()
        .filter(|binding| !overridden.contains(binding.command.as_str()))
        .cloned()
        .collect();
    merged.extend(custom);
    keep_last_max(merged)
}

fn keep_last_max(mut rules: Vec<ResolvedKeybindingRule>) -> Vec<ResolvedKeybindingRule> {
    if rules.len() > MAX_KEYBINDINGS_COUNT {
        rules.drain(..rules.len() - MAX_KEYBINDINGS_COUNT);
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(key: &str) -> Option<KeybindingShortcut> {
        parse_keybinding_shortcut(key)
    }

    #[test]
    fn parses_modifiers_and_aliases() {
        let parsed = shortcut("Cmd+Control+Option+Shift+Mod+K").unwrap();
        assert_eq!(parsed.key.as_str(), "k");
        assert!(parsed.meta_key && parsed.ctrl_key && parsed.alt_key);
        assert!(parsed.shift_key && parsed.mod_key);
        assert_eq!(shortcut("esc").unwrap().key.as_str(), "escape");
        // Deviation: upstream maps `space` to a " " key, which `KeybindingValue`
        // (trimmed, min length 1) cannot hold. Such rules are dropped.
        assert!(shortcut("mod+space").is_none());
    }

    #[test]
    fn a_trailing_plus_is_the_plus_key() {
        assert_eq!(shortcut("mod++").unwrap().key.as_str(), "+");
        assert_eq!(shortcut("+").unwrap().key.as_str(), "+");
    }

    #[test]
    fn rejects_empty_tokens_and_double_keys() {
        // Upstream quirk kept: "" and "+" both parse to the "+" key.
        assert_eq!(shortcut("").unwrap().key.as_str(), "+");
        assert!(shortcut("mod+").is_some());
        assert!(shortcut("a+b").is_none());
        assert!(shortcut("mod++a").is_none());
        assert!(shortcut("mod").is_none());
    }

    #[test]
    fn parses_when_with_precedence() {
        // `!a && b || c` is `((!a) && b) || c`.
        let ast = parse_keybinding_when_expression("!a && b || c").unwrap();
        let expected = KeybindingWhenNode::Or {
            left: Box::new(KeybindingWhenNode::And {
                left: Box::new(KeybindingWhenNode::Not {
                    node: Box::new(KeybindingWhenNode::Identifier { name: "a".into() }),
                }),
                right: Box::new(KeybindingWhenNode::Identifier { name: "b".into() }),
            }),
            right: Box::new(KeybindingWhenNode::Identifier { name: "c".into() }),
        };
        assert_eq!(ast, expected);
    }

    #[test]
    fn rejects_malformed_when_expressions() {
        for expression in ["", "a &&", "(a", "a)", "a b", "1a", "a & b", "!"] {
            assert!(
                parse_keybinding_when_expression(expression).is_none(),
                "{expression}"
            );
        }
    }

    #[test]
    fn rejects_when_nesting_beyond_the_depth_limit() {
        let deep = format!(
            "{}a{}",
            "(".repeat(MAX_WHEN_EXPRESSION_DEPTH + 2),
            ")".repeat(MAX_WHEN_EXPRESSION_DEPTH + 2)
        );
        assert!(parse_keybinding_when_expression(&deep).is_none());
    }

    #[test]
    fn default_keybindings_all_compile() {
        assert_eq!(
            compile_resolved_keybindings_config(&DEFAULT_KEYBINDINGS).len(),
            DEFAULT_KEYBINDINGS.len()
        );
    }

    #[test]
    fn merge_drops_defaults_for_rebound_commands_and_keeps_custom_last() {
        let custom =
            compile_resolved_keybindings_config(&[rule("mod+alt+x", "sidebar.toggle", None)]);
        let merged = merge_with_default_keybindings(custom.clone());
        assert_eq!(merged.last().unwrap(), &custom[0]);
        assert_eq!(
            merged
                .iter()
                .filter(|binding| binding.command.as_str() == "sidebar.toggle")
                .count(),
            1
        );
    }

    #[test]
    fn empty_custom_config_is_the_default_set() {
        assert_eq!(
            merge_with_default_keybindings(Vec::new()),
            *DEFAULT_RESOLVED_KEYBINDINGS
        );
    }
}
