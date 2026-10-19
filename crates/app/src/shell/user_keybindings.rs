//! Keyboard shortcuts follow the keybindings config (`keybindings.json`).
//!
//! Upstream matches each key event against the resolved rules at run time
//! (`resolveShortcutCommand`). GPUI dispatches through a keymap of static
//! bindings instead, so this keeps that keymap in step with the rules: the
//! built-in bindings are the defaults, and each change of the resolved rules
//! (Settings > Keybindings, or an edit of the file) un-binds the shortcuts that
//! went away and binds the new ones, later bindings outranking earlier ones.
//! The same rules feed the shortcut labels (palette, composer hints, usage).
//!
//! A command keeps the key context its built-in binding has; `when`
//! expressions are not evaluated (PORTING.md Deviations).

use std::collections::BTreeSet;
use std::sync::RwLock;

use futures::StreamExt as _;
use gpui_kit::{Action as _, App, Global, KeyBinding, Keystroke, SharedString, Unbind};
use turin_shared::keybindings::{
    ParsedShortcut, ResolvedKeybindingRule, compile_resolved_keybindings_config,
    default_keybinding_rules,
};

use crate::app_runtime::write::bus::CommandBus;
use crate::components::app_sidebar_layout::{
    FindInThread, OpenUsage, ToggleRightPanel, ToggleSidebar, ToggleThreadDetails, UndoLatestThread,
};
use crate::components::command_palette::{
    CycleAppearance, SelectTheme, ToggleCommandPalette, ToggleFilePicker, ToggleProjectSearch,
};
use crate::components::sidebar::{
    NewThread, NewThreadInProject, PinThread, SettleThread, ThreadJump1, ThreadJump2, ThreadJump3,
    ThreadJump4, ThreadJump5, ThreadJump6, ThreadJump7, ThreadJump8, ThreadJump9, ThreadNext,
    ThreadPrevious,
};
use crate::shell::chat_route_shortcuts::ReopenClosedView;

/// The key context every shell command is bound in.
const SHELL_CONTEXT: &str = "AppShell";

/// The resolved rules in force, once the config has been read.
static ACTIVE: RwLock<Option<Vec<ResolvedKeybindingRule>>> = RwLock::new(None);

/// The rules shortcuts and labels follow: the config's, or the defaults
/// before it has been read.
pub fn active_rules() -> Vec<ResolvedKeybindingRule> {
    ACTIVE
        .read()
        .ok()
        .and_then(|rules| rules.clone())
        .unwrap_or_else(default_rules)
}

fn default_rules() -> Vec<ResolvedKeybindingRule> {
    compile_resolved_keybindings_config(&default_keybinding_rules())
}

fn set_active(rules: Vec<ResolvedKeybindingRule>) {
    if let Ok(mut active) = ACTIVE.write() {
        *active = Some(rules);
    }
}

/// How one command is bound: its action, in the shell's key context.
struct CommandAction {
    command: &'static str,
    bind: fn(&str) -> KeyBinding,
    unbind: fn(&str) -> KeyBinding,
}

macro_rules! command {
    ($command:literal, $action:expr) => {
        CommandAction {
            command: $command,
            bind: |keystroke| KeyBinding::new(keystroke, $action, Some(SHELL_CONTEXT)),
            unbind: |keystroke| {
                KeyBinding::new(
                    keystroke,
                    Unbind(SharedString::from($action.name())),
                    Some(SHELL_CONTEXT),
                )
            },
        }
    };
}

/// Every command with a GPUI action.
fn command_actions() -> Vec<CommandAction> {
    vec![
        command!("sidebar.toggle", ToggleSidebar),
        command!("rightPanel.toggle", ToggleRightPanel),
        command!("threadPanel.toggle", ToggleThreadDetails),
        command!("usage.open", OpenUsage),
        command!("thread.undo", UndoLatestThread),
        command!("chat.find", FindInThread),
        command!("commandPalette.toggle", ToggleCommandPalette),
        command!("filePicker.toggle", ToggleFilePicker),
        command!("projectSearch.toggle", ToggleProjectSearch),
        command!("theme.select", SelectTheme),
        command!("appearance.cycle", CycleAppearance),
        command!("view.reopenClosed", ReopenClosedView),
        command!("thread.previous", ThreadPrevious),
        command!("thread.next", ThreadNext),
        command!("thread.settle", SettleThread),
        command!("thread.pin", PinThread),
        command!("chat.new", NewThread),
        command!("chat.newLocal", NewThreadInProject),
        command!("thread.jump.1", ThreadJump1),
        command!("thread.jump.2", ThreadJump2),
        command!("thread.jump.3", ThreadJump3),
        command!("thread.jump.4", ThreadJump4),
        command!("thread.jump.5", ThreadJump5),
        command!("thread.jump.6", ThreadJump6),
        command!("thread.jump.7", ThreadJump7),
        command!("thread.jump.8", ThreadJump8),
        command!("thread.jump.9", ThreadJump9),
    ]
}

/// A rule's shortcut as a GPUI keystroke (`cmd-shift-[`), `None` when GPUI has
/// no spelling for its key.
pub fn gpui_keystroke(shortcut: &ParsedShortcut, mac: bool) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if shortcut.ctrl_key || (shortcut.mod_key && !mac) {
        parts.push("ctrl");
    }
    if shortcut.alt_key {
        parts.push("alt");
    }
    if shortcut.shift_key {
        parts.push("shift");
    }
    if shortcut.meta_key || (shortcut.mod_key && mac) {
        parts.push("cmd");
    }
    let key = match shortcut.key.as_str() {
        " " => "space",
        "arrowup" => "up",
        "arrowdown" => "down",
        "arrowleft" => "left",
        "arrowright" => "right",
        other => other,
    };
    parts.push(key);
    let keystroke = parts.join("-");
    Keystroke::parse(&keystroke).ok()?;
    Some(keystroke)
}

/// The `(command, keystroke)` pairs `rules` bind among the commands with an
/// action.
fn bound_pairs(rules: &[ResolvedKeybindingRule], mac: bool) -> BTreeSet<(String, String)> {
    let known: BTreeSet<&str> = command_actions()
        .iter()
        .map(|action| action.command)
        .collect();
    rules
        .iter()
        .filter(|rule| known.contains(rule.command.as_str()))
        .filter_map(|rule| {
            gpui_keystroke(&rule.shortcut, mac)
                .map(|keystroke| (rule.command.as_str().to_owned(), keystroke))
        })
        .collect()
}

/// A command and the GPUI keystroke it is bound to.
type Pair = (String, String);

/// What to change to go from the bindings `applied` to `wanted`: the pairs to
/// un-bind, then the pairs to bind.
fn plan(
    applied: &BTreeSet<(String, String)>,
    wanted: &BTreeSet<(String, String)>,
) -> (Vec<Pair>, Vec<Pair>) {
    (
        applied.difference(wanted).cloned().collect(),
        wanted.difference(applied).cloned().collect(),
    )
}

/// The bindings the app starts with (the static ones follow the defaults).
struct AppliedBindings(BTreeSet<(String, String)>);

impl Global for AppliedBindings {}

fn is_mac() -> bool {
    cfg!(target_os = "macos")
}

/// The keymap entries for a [`plan`]: un-bindings first, then bindings, so
/// a shortcut that moves between commands ends up with its new command.
fn bindings_for_plan(remove: &[(String, String)], add: &[(String, String)]) -> Vec<KeyBinding> {
    let actions = command_actions();
    let find = |command: &str| actions.iter().find(|action| action.command == command);
    remove
        .iter()
        .filter_map(|(command, keystroke)| find(command).map(|action| (action.unbind)(keystroke)))
        .chain(add.iter().filter_map(|(command, keystroke)| {
            find(command).map(|action| (action.bind)(keystroke))
        }))
        .collect()
}

/// Makes the keymap and the labels follow `rules`.
pub fn apply_rules(rules: Vec<ResolvedKeybindingRule>, cx: &mut App) {
    let mac = is_mac();
    let wanted = bound_pairs(&rules, mac);
    let applied = cx
        .try_global::<AppliedBindings>()
        .map(|applied| applied.0.clone())
        .unwrap_or_else(|| bound_pairs(&default_rules(), mac));
    let (remove, add) = plan(&applied, &wanted);
    let bindings = bindings_for_plan(&remove, &add);
    set_active(rules);
    cx.set_global(AppliedBindings(wanted));
    if !bindings.is_empty() {
        cx.bind_keys(bindings);
    }
}

/// Follows the keybindings service for as long as the app runs.
pub fn install(bus: CommandBus, cx: &mut App) {
    let (updates, mut applied) = futures::channel::mpsc::unbounded::<Vec<ResolvedKeybindingRule>>();
    let runtime = bus.handle().clone();
    runtime.spawn(async move {
        let commands = loop {
            match bus.commands() {
                Ok(commands) => break commands,
                Err(error) if error.message.starts_with("Turin could not start") => return,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
            }
        };
        let keybindings = commands.runtime().keybindings.clone();
        let mut changes = keybindings.subscribe();
        if let Ok(state) = keybindings.snapshot().await
            && updates.unbounded_send(rules_of(&state)).is_err()
        {
            return;
        }
        while let Some(state) = changes.recv().await {
            if updates.unbounded_send(rules_of(&state)).is_err() {
                return;
            }
        }
    });
    cx.spawn(async move |cx| {
        while let Some(rules) = applied.next().await {
            cx.update(|cx| apply_rules(rules, cx));
        }
    })
    .detach();
}

fn rules_of(
    state: &turin_runtime::keybindings::KeybindingsConfigState,
) -> Vec<ResolvedKeybindingRule> {
    state
        .keybindings
        .iter()
        .filter_map(crate::settings::live_snapshot::resolved_keybinding_rule)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::keybindings::KeybindingRule;
    use turin_shared::keybindings::{merge_with_default_keybindings, parse_keybinding_shortcut};

    fn rule(key: &str, command: &str) -> KeybindingRule {
        serde_json::from_value(serde_json::json!({"key": key, "command": command})).expect("rule")
    }

    fn pair(command: &str, keystroke: &str) -> (String, String) {
        (command.to_owned(), keystroke.to_owned())
    }

    #[test]
    fn shortcuts_spell_as_gpui_keystrokes() {
        let spell = |value: &str, mac| {
            gpui_keystroke(&parse_keybinding_shortcut(value).expect("parses"), mac)
        };
        assert_eq!(spell("mod+b", true).as_deref(), Some("cmd-b"));
        assert_eq!(spell("mod+b", false).as_deref(), Some("ctrl-b"));
        assert_eq!(spell("mod+shift+[", true).as_deref(), Some("shift-cmd-["));
        assert_eq!(
            spell("mod+alt+shift+a", true).as_deref(),
            Some("alt-shift-cmd-a")
        );
        assert_eq!(spell("alt+arrowup", true).as_deref(), Some("alt-up"));
        assert_eq!(spell("mod+enter", true).as_deref(), Some("cmd-enter"));
    }

    #[test]
    fn the_defaults_bind_the_shells_shortcuts() {
        let pairs = bound_pairs(&default_rules(), true);
        for expected in [
            pair("sidebar.toggle", "cmd-b"),
            pair("commandPalette.toggle", "cmd-k"),
            pair("chat.new", "cmd-n"),
            pair("chat.new", "shift-cmd-o"),
            pair("thread.jump.3", "cmd-3"),
            pair("view.reopenClosed", "shift-cmd-t"),
        ] {
            assert!(pairs.contains(&expected), "{expected:?} in {pairs:?}");
        }
        // A command without an action is not bound here.
        assert!(pairs.iter().all(|(command, _)| command != "composer.stash"));
    }

    #[test]
    fn every_command_with_an_action_has_a_default_shortcut() {
        let pairs = bound_pairs(&default_rules(), true);
        for action in command_actions() {
            assert!(
                pairs.iter().any(|(command, _)| command == action.command),
                "{} has no default shortcut",
                action.command
            );
        }
    }

    #[test]
    fn rebinding_a_command_unbinds_its_old_shortcut_and_binds_the_new_one() {
        let applied = bound_pairs(&default_rules(), true);
        let custom = compile_resolved_keybindings_config(&[rule("mod+shift+y", "sidebar.toggle")]);
        let wanted = bound_pairs(&merge_with_default_keybindings(&custom), true);
        let (remove, add) = plan(&applied, &wanted);
        assert_eq!(remove, vec![pair("sidebar.toggle", "cmd-b")]);
        assert_eq!(add, vec![pair("sidebar.toggle", "shift-cmd-y")]);
    }

    #[test]
    fn removing_the_custom_rule_brings_the_default_back() {
        let custom = compile_resolved_keybindings_config(&[rule("mod+shift+y", "sidebar.toggle")]);
        let applied = bound_pairs(&merge_with_default_keybindings(&custom), true);
        let wanted = bound_pairs(&default_rules(), true);
        let (remove, add) = plan(&applied, &wanted);
        assert_eq!(remove, vec![pair("sidebar.toggle", "shift-cmd-y")]);
        assert_eq!(add, vec![pair("sidebar.toggle", "cmd-b")]);
    }

    /// What the keymap runs for `keystroke` in the shell's key context.
    fn runs(keymap: &gpui_kit::Keymap, keystroke: &str) -> Vec<String> {
        let context = gpui_kit::KeyContext::parse(SHELL_CONTEXT).expect("context");
        let typed = [Keystroke::parse(keystroke).expect("keystroke")];
        keymap
            .bindings_for_input(&typed, &[context])
            .0
            .iter()
            .map(|binding| binding.action().name().to_owned())
            .collect()
    }

    #[test]
    fn the_keymap_runs_the_rebound_shortcut_and_not_the_old_one() {
        let defaults = bound_pairs(&default_rules(), true);
        let mut keymap = gpui_kit::Keymap::new(bindings_for_plan(
            &Vec::new(),
            &defaults.iter().cloned().collect::<Vec<_>>(),
        ));
        assert_eq!(runs(&keymap, "cmd-b"), ["turin::ToggleSidebar"]);

        let custom = compile_resolved_keybindings_config(&[rule("mod+shift+y", "sidebar.toggle")]);
        let wanted = bound_pairs(&merge_with_default_keybindings(&custom), true);
        let (remove, add) = plan(&defaults, &wanted);
        keymap.add_bindings(bindings_for_plan(&remove, &add));
        assert!(runs(&keymap, "cmd-b").is_empty());
        assert_eq!(runs(&keymap, "shift-cmd-y"), ["turin::ToggleSidebar"]);
        // Other shortcuts are untouched.
        assert_eq!(
            runs(&keymap, "cmd-k"),
            ["command_palette::ToggleCommandPalette"]
        );

        // Putting the default back runs it again, and the moved one no longer runs.
        let (remove, add) = plan(&wanted, &defaults);
        keymap.add_bindings(bindings_for_plan(&remove, &add));
        assert_eq!(runs(&keymap, "cmd-b"), ["turin::ToggleSidebar"]);
        assert!(runs(&keymap, "shift-cmd-y").is_empty());
    }

    #[test]
    fn unchanged_rules_change_nothing() {
        let applied = bound_pairs(&default_rules(), true);
        assert_eq!(plan(&applied, &applied), (Vec::new(), Vec::new()));
    }

    #[test]
    fn labels_follow_the_rules_in_force() {
        use crate::components::command_palette::shortcut_labels::shortcut_label_for_command_in;
        let custom = compile_resolved_keybindings_config(&[rule("mod+shift+y", "sidebar.toggle")]);
        let rules = merge_with_default_keybindings(&custom);
        assert_eq!(
            shortcut_label_for_command_in(&rules, "sidebar.toggle", true).as_deref(),
            Some("\u{21e7}\u{2318}Y")
        );
        assert_eq!(
            shortcut_label_for_command_in(&default_rules(), "sidebar.toggle", true).as_deref(),
            Some("\u{2318}B")
        );
    }
}
