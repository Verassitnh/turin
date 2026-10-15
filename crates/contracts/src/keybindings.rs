//! Port of upstream/packages/contracts/src/keybindings.ts

use crate::base_schemas::{ContractError, TrimmedString, js_len, js_trim};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const MAX_KEYBINDING_VALUE_LENGTH: usize = 64;
const MAX_KEYBINDING_WHEN_LENGTH: usize = 256;
pub const MAX_WHEN_EXPRESSION_DEPTH: usize = 64;
pub const MAX_SCRIPT_ID_LENGTH: usize = 24;
pub const MAX_KEYBINDINGS_COUNT: usize = 256;

pub const THREAD_JUMP_KEYBINDING_COMMANDS: &[&str] = &[
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

pub const MODEL_PICKER_JUMP_KEYBINDING_COMMANDS: &[&str] = &[
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
];

/// The thread command group (folded into `STATIC_KEYBINDING_COMMANDS`).
#[allow(dead_code)]
const THREAD_KEYBINDING_COMMANDS: &[&str] = &[
    "thread.stop",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.copyReference",
    "thread.settle",
    "thread.pin",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// The model-picker command group (folded into `STATIC_KEYBINDING_COMMANDS`).
#[allow(dead_code)]
const MODEL_PICKER_KEYBINDING_COMMANDS: &[&str] = &[
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
];

pub const STATIC_KEYBINDING_COMMANDS: &[&str] = &[
    "sidebar.toggle",
    "navigation.back",
    "navigation.forward",
    "terminal.toggle",
    "terminal.split",
    "terminal.splitVertical",
    "terminal.new",
    "terminal.close",
    "rightPanel.toggle",
    "rightPanel.new",
    "threadPanel.toggle",
    "rightPanel.toggleMaximized",
    "rightPanel.close",
    "view.reopenClosed",
    "pullRequest.copyNumber",
    "diff.toggle",
    "preview.toggle",
    "preview.refresh",
    "preview.focusUrl",
    "preview.zoomIn",
    "preview.zoomOut",
    "preview.resetZoom",
    "commandPalette.toggle",
    "filePicker.toggle",
    "projectSearch.toggle",
    "usage.open",
    "theme.select",
    "appearance.cycle",
    "themeEditor.toggle",
    "composer.stash",
    "composer.sendAlternate",
    "composer.sendBackground",
    "composer.sendAndNewThread",
    "composer.host",
    "composer.cycleHost",
    "composer.effort",
    "composer.mode",
    "composer.workspace",
    "composer.previousWorktree",
    "composer.branch",
    "chat.new",
    "chat.newLocal",
    "chat.newWithoutProject",
    "chat.find",
    "editor.openFavorite",
    "usage.cost",
    "usage.tokens",
    "usage.limits",
    "usage.period.day",
    "usage.period.week",
    "usage.period.month",
    "usage.period.quarter",
    "modelPicker.toggle",
    "modelPicker.previousProvider",
    "modelPicker.nextProvider",
    "modelPicker.jump.1",
    "modelPicker.jump.2",
    "modelPicker.jump.3",
    "modelPicker.jump.4",
    "modelPicker.jump.5",
    "modelPicker.jump.6",
    "modelPicker.jump.7",
    "modelPicker.jump.8",
    "modelPicker.jump.9",
    "thread.stop",
    "thread.steerQueuedMessage",
    "thread.editQueuedMessage",
    "thread.previous",
    "thread.next",
    "thread.copyReference",
    "thread.settle",
    "thread.pin",
    "thread.undo",
    "thread.jump.1",
    "thread.jump.2",
    "thread.jump.3",
    "thread.jump.4",
    "thread.jump.5",
    "thread.jump.6",
    "thread.jump.7",
    "thread.jump.8",
    "thread.jump.9",
];

/// `script.<id>.run` where `<id>` is `^[a-z0-9][a-z0-9-]*$` (≤ 24 chars).
fn is_script_run_command(value: &str) -> bool {
    let Some(id) = value
        .strip_prefix("script.")
        .and_then(|rest| rest.strip_suffix(".run"))
    else {
        return false;
    };
    let bytes = id.as_bytes();
    !id.is_empty()
        && id.len() <= MAX_SCRIPT_ID_LENGTH
        && bytes[0].is_ascii_lowercase_or_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

trait AsciiLowercaseOrDigit {
    fn is_ascii_lowercase_or_digit(&self) -> bool;
}

impl AsciiLowercaseOrDigit for u8 {
    fn is_ascii_lowercase_or_digit(&self) -> bool {
        self.is_ascii_lowercase() || self.is_ascii_digit()
    }
}

/// `KeybindingCommand`: a static command literal or `script.<id>.run`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeybindingCommand(String);

impl KeybindingCommand {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        // Literals and the template pattern match the raw string: no trimming.
        let value = value.as_ref();
        let valid = STATIC_KEYBINDING_COMMANDS.contains(&value) || is_script_run_command(value);
        if !valid {
            return Err(ContractError {
                expected: "a known keybinding command or a script.<id>.run pattern",
                got: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
    pub fn is_script_run(&self) -> bool {
        is_script_run_command(&self.0)
    }
}

impl fmt::Display for KeybindingCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for KeybindingCommand {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for KeybindingCommand {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for KeybindingCommand {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        KeybindingCommand::new(<std::borrow::Cow<'de, str>>::deserialize(d)?)
            .map_err(serde::de::Error::custom)
    }
}

/// `KeybindingValue`: trimmed, 1..=64 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeybindingValue(TrimmedString);

impl KeybindingValue {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        if trimmed.is_empty() || js_len(trimmed) > MAX_KEYBINDING_VALUE_LENGTH {
            return Err(ContractError {
                expected: "a keybinding value of 1..=64 chars",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(TrimmedString::new(trimmed)))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for KeybindingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for KeybindingValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for KeybindingValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// `KeybindingWhen`: trimmed, 1..=256 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeybindingWhen(TrimmedString);

impl KeybindingWhen {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        if trimmed.is_empty() || js_len(trimmed) > MAX_KEYBINDING_WHEN_LENGTH {
            return Err(ContractError {
                expected: "a when expression of 1..=256 chars",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(TrimmedString::new(trimmed)))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for KeybindingWhen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for KeybindingWhen {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl<'de> Deserialize<'de> for KeybindingWhen {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingRule {
    pub key: KeybindingValue,
    pub command: KeybindingCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<KeybindingWhen>,
}

/// `KeybindingsConfig`: at most [`MAX_KEYBINDINGS_COUNT`] rules. Apply the
/// `keybindings_config` module on fields that decode it.
pub type KeybindingsConfig = Vec<KeybindingRule>;

/// Serde codec for [`KeybindingsConfig`] with upstream's count cap.
pub mod keybindings_config {
    use super::*;

    pub fn serialize<S: Serializer>(rules: &[KeybindingRule], s: S) -> Result<S::Ok, S::Error> {
        rules.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<KeybindingRule>, D::Error> {
        let rules = Vec::<KeybindingRule>::deserialize(d)?;
        if rules.len() > MAX_KEYBINDINGS_COUNT {
            return Err(serde::de::Error::custom(format!(
                "keybindings are limited to {MAX_KEYBINDINGS_COUNT} rules"
            )));
        }
        Ok(rules)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingShortcut {
    pub key: KeybindingValue,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub mod_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewForwardedShortcut {
    pub command: KeybindingCommand,
    pub shortcut: KeybindingShortcut,
}

fn non_empty_name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let name = String::deserialize(d)?;
    if name.is_empty() {
        Err(serde::de::Error::custom("expected a non-empty string"))
    } else {
        Ok(name)
    }
}

/// AST of a `when` expression (recursive union on `type`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KeybindingWhenNode {
    Identifier {
        #[serde(deserialize_with = "non_empty_name")]
        name: String,
    },
    Not {
        node: Box<KeybindingWhenNode>,
    },
    And {
        left: Box<KeybindingWhenNode>,
        right: Box<KeybindingWhenNode>,
    },
    Or {
        left: Box<KeybindingWhenNode>,
        right: Box<KeybindingWhenNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedKeybindingRule {
    pub command: KeybindingCommand,
    pub shortcut: KeybindingShortcut,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_ast: Option<KeybindingWhenNode>,
}

/// The command set grows over time, so a client may receive rules it cannot
/// represent (a command or `when` node added after that client shipped).
/// Decoding drops those rules instead of failing the whole payload —
/// rejecting the config would take down the connection over a shortcut the
/// client couldn't dispatch anyway. Capped at [`MAX_KEYBINDINGS_COUNT`].
pub type ResolvedKeybindingsConfig = Vec<ResolvedKeybindingRule>;

pub mod resolved_keybindings_config {
    use super::*;

    pub fn serialize<S: Serializer>(
        rules: &[ResolvedKeybindingRule],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        rules.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ResolvedKeybindingRule>, D::Error> {
        let raw = Vec::<serde_json::Value>::deserialize(d)?;
        let rules: Vec<ResolvedKeybindingRule> = raw
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();
        if rules.len() > MAX_KEYBINDINGS_COUNT {
            return Err(serde::de::Error::custom(format!(
                "keybindings are limited to {MAX_KEYBINDINGS_COUNT} rules"
            )));
        }
        Ok(rules)
    }
}

/// `KeybindingsConfigError` (upstream TaggedError tag is
/// `KeybindingsConfigParseError`, deliberately not the class name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum KeybindingsConfigError {
    #[serde(rename = "KeybindingsConfigParseError")]
    #[error("Unable to parse keybindings config at {config_path}: {detail}")]
    ParseError {
        config_path: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keybinding_rules() {
        for (key, command) in [
            ("mod+j", "terminal.toggle"),
            ("mod+b", "sidebar.toggle"),
            ("mod+alt+b", "rightPanel.toggle"),
            ("mod+shift+b", "threadPanel.toggle"),
            ("mod+shift+m", "rightPanel.toggleMaximized"),
            ("mod+w", "terminal.close"),
        ] {
            let rule: KeybindingRule =
                serde_json::from_value(serde_json::json!({ "key": key, "command": command }))
                    .unwrap_or_else(|e| panic!("{command}: {e}"));
            assert_eq!(rule.command.as_str(), command);
            assert_eq!(rule.when, None);
        }
    }

    #[test]
    fn accepts_script_run_commands() {
        let rule: KeybindingRule = serde_json::from_value(serde_json::json!({
            "key": "mod+r",
            "command": "script.run-tests.run",
        }))
        .unwrap();
        assert_eq!(rule.command.as_str(), "script.run-tests.run");
        assert!(rule.command.is_script_run());

        assert!(
            serde_json::from_value::<KeybindingRule>(serde_json::json!({
                "key": "mod+r",
                "command": "script.Bad_ID.run",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeybindingRule>(serde_json::json!({
                "key": "mod+r",
                "command": "unknown.command",
            }))
            .is_err()
        );
    }

    #[test]
    fn rejects_blank_and_overlong_keys() {
        assert!(
            serde_json::from_value::<KeybindingRule>(serde_json::json!({
                "key": "   ", "command": "terminal.toggle",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeybindingRule>(serde_json::json!({
                "key": "a".repeat(65), "command": "terminal.toggle",
            }))
            .is_err()
        );
    }

    #[test]
    fn resolved_config_drops_undecodable_rules() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(with = "resolved_keybindings_config")]
            rules: Vec<ResolvedKeybindingRule>,
        }
        let holder: Holder = serde_json::from_value(serde_json::json!({
            "rules": [
                { "command": "terminal.toggle",
                  "shortcut": { "key": "mod+j", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
                { "command": "not.a.command",
                  "shortcut": { "key": "mod+k", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } }
            ]
        }))
        .unwrap();
        assert_eq!(holder.rules.len(), 1);
        assert_eq!(holder.rules[0].command.as_str(), "terminal.toggle");
    }

    use serde_json::{Value, json};

    fn shortcut() -> Value {
        json!({ "key": "p", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true })
    }

    fn resolved(value: Value) -> Vec<ResolvedKeybindingRule> {
        #[derive(Deserialize)]
        struct Holder(#[serde(with = "resolved_keybindings_config")] Vec<ResolvedKeybindingRule>);
        serde_json::from_value::<Holder>(value).unwrap().0
    }

    #[test]
    fn every_static_command_decodes_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for command in STATIC_KEYBINDING_COMMANDS {
            assert!(seen.insert(*command), "duplicate {command}");
            let rule: KeybindingRule =
                serde_json::from_value(json!({ "key": "mod+x", "command": command })).unwrap();
            assert_eq!(
                serde_json::to_value(&rule).unwrap()["command"],
                json!(command)
            );
        }
        assert_eq!(STATIC_KEYBINDING_COMMANDS.len(), 82);
        assert!(
            THREAD_JUMP_KEYBINDING_COMMANDS
                .iter()
                .all(|c| STATIC_KEYBINDING_COMMANDS.contains(c))
        );
        assert!(
            MODEL_PICKER_JUMP_KEYBINDING_COMMANDS
                .iter()
                .all(|c| STATIC_KEYBINDING_COMMANDS.contains(c))
        );
    }

    #[test]
    fn commands_are_matched_verbatim_and_script_ids_are_bounded() {
        assert!(KeybindingCommand::new(" terminal.toggle ").is_err());
        assert!(KeybindingCommand::new("script.Test.run").is_err());
        assert!(KeybindingCommand::new("script..run").is_err());
        assert!(KeybindingCommand::new("script.-a.run").is_err());
        assert!(KeybindingCommand::new("script.setup.run").is_ok());
        assert!(KeybindingCommand::new(format!("script.{}.run", "a".repeat(24))).is_ok());
        assert!(KeybindingCommand::new(format!("script.{}.run", "a".repeat(25))).is_err());
    }

    #[test]
    fn rule_values_are_trimmed_and_when_is_optional() {
        let rule: KeybindingRule = serde_json::from_value(json!({
            "key": "  mod+shift+s ", "command": "thread.settle", "when": " !terminalFocus ",
        }))
        .unwrap();
        assert_eq!(rule.key.as_str(), "mod+shift+s");
        assert_eq!(rule.when.as_ref().unwrap().as_str(), "!terminalFocus");
        assert!(
            serde_json::from_value::<KeybindingRule>(
                json!({ "key": "k", "command": "thread.settle", "when": "   " })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<KeybindingRule>(
                json!({ "key": "k", "command": "thread.settle", "when": "w".repeat(257) })
            )
            .is_err()
        );
    }

    #[test]
    fn config_array_is_capped() {
        #[derive(Deserialize)]
        struct Holder(#[serde(with = "keybindings_config")] Vec<KeybindingRule>);
        let rules = |n: usize| vec![json!({ "key": "k", "command": "terminal.toggle" }); n];
        assert_eq!(
            serde_json::from_value::<Holder>(json!(rules(3)))
                .unwrap()
                .0
                .len(),
            3
        );
        assert!(serde_json::from_value::<Holder>(json!(rules(256))).is_ok());
        assert!(serde_json::from_value::<Holder>(json!(rules(257))).is_err());
    }

    #[test]
    fn resolved_rules_parse_with_when_ast() {
        let parsed = resolved(json!([{
            "command": "terminal.split",
            "shortcut": shortcut(),
            "whenAst": {
                "type": "and",
                "left": { "type": "identifier", "name": "terminalOpen" },
                "right": { "type": "not", "node": { "type": "identifier", "name": "terminalFocus" } },
            },
        }]));
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0].when_ast,
            Some(KeybindingWhenNode::And { .. })
        ));
        assert!(
            serde_json::from_value::<KeybindingWhenNode>(
                json!({ "type": "identifier", "name": "" })
            )
            .is_err()
        );
    }

    #[test]
    fn resolved_config_drops_unknown_commands_when_nodes_and_garbage() {
        let parsed = resolved(json!([
            { "command": "terminal.toggle", "shortcut": shortcut() },
            { "command": "someFuture.toggle", "shortcut": shortcut() },
            { "command": "terminal.split", "shortcut": shortcut(), "whenAst": { "type": "xor", "left": 1, "right": 2 } },
            "garbage",
            null,
            { "command": "filePicker.toggle", "shortcut": shortcut() },
        ]));
        let commands: Vec<&str> = parsed.iter().map(|rule| rule.command.as_str()).collect();
        assert_eq!(commands, ["terminal.toggle", "filePicker.toggle"]);
    }

    #[test]
    fn resolved_rules_encode_plain_and_ignore_unknown_fields() {
        let input = json!([{ "command": "terminal.toggle", "shortcut": shortcut() }]);
        let parsed = resolved(input.clone());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), input);
        let with_extra = resolved(
            json!([{ "command": "terminal.toggle", "shortcut": shortcut(), "key": "mod+j" }]),
        );
        assert_eq!(serde_json::to_value(&with_extra).unwrap(), input);
    }

    #[test]
    fn parse_error_message_and_tag() {
        let error: KeybindingsConfigError = serde_json::from_value(json!({
            "_tag": "KeybindingsConfigParseError", "configPath": "/k.json", "detail": "bad",
        }))
        .unwrap();
        assert_eq!(
            error.to_string(),
            "Unable to parse keybindings config at /k.json: bad"
        );
    }
}
