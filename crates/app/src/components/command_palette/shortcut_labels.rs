//! The slice of upstream/apps/web/src/keybindings.ts the palette reads:
//! `formatShortcutKeyLabel`, `formatShortcutLabel` and
//! `shortcutLabelForCommand` over the default keybindings. T20 (settings)
//! owns user-customised bindings; until it feeds resolved rules in, labels
//! come from the defaults (packages/shared/src/keybindings.ts).
//!
//! Port of upstream/apps/web/src/keybindings.ts (the slice the palette reads).

use turin_shared::keybindings::{ParsedShortcut, ResolvedKeybindingRule};

/// `formatShortcutKeyLabel`.
pub fn format_shortcut_key_label(key: &str) -> String {
    if key == " " {
        return "Space".into();
    }
    if key.chars().count() == 1 {
        return key.to_uppercase();
    }
    match key {
        "escape" => "Esc".into(),
        "arrowup" => "Up".into(),
        "arrowdown" => "Down".into(),
        "arrowleft" => "Left".into(),
        "arrowright" => "Right".into(),
        _ => {
            let mut chars = key.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

/// `formatShortcutLabel`.
pub fn format_shortcut_label(shortcut: &ParsedShortcut, mac: bool) -> String {
    let key_label = format_shortcut_key_label(shortcut.key.as_str());
    let show_meta = shortcut.meta_key || (shortcut.mod_key && mac);
    let show_ctrl = shortcut.ctrl_key || (shortcut.mod_key && !mac);
    let show_alt = shortcut.alt_key;
    let show_shift = shortcut.shift_key;

    if mac {
        return format!(
            "{}{}{}{}{}",
            if show_ctrl { "\u{2303}" } else { "" },
            if show_alt { "\u{2325}" } else { "" },
            if show_shift { "\u{21e7}" } else { "" },
            if show_meta { "\u{2318}" } else { "" },
            key_label
        );
    }

    let mut parts: Vec<&str> = Vec::new();
    if show_ctrl {
        parts.push("Ctrl");
    }
    if show_alt {
        parts.push("Alt");
    }
    if show_shift {
        parts.push("Shift");
    }
    if show_meta {
        parts.push("Meta");
    }
    parts.push(&key_label);
    parts.join("+")
}

/// `shortcutLabelForCommand` over `rules`: the last rule for the command wins,
/// as upstream's effective-shortcut lookup does.
pub fn shortcut_label_for_command_in(
    rules: &[ResolvedKeybindingRule],
    command: &str,
    mac: bool,
) -> Option<String> {
    rules
        .iter()
        .rev()
        .find(|rule| rule.command.as_str() == command)
        .map(|rule| format_shortcut_label(&rule.shortcut, mac))
}

/// The label of the shortcut in force for `command`: the keybindings config's
/// (Settings > Keybindings), the defaults before it has been read.
pub fn default_shortcut_label_for_command(command: &str, mac: bool) -> Option<String> {
    shortcut_label_for_command_in(
        &crate::shell::user_keybindings::active_rules(),
        command,
        mac,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_default_shortcuts_the_mac_way() {
        assert_eq!(
            default_shortcut_label_for_command("commandPalette.toggle", true).as_deref(),
            Some("\u{2318}K")
        );
        assert_eq!(
            default_shortcut_label_for_command("chat.newWithoutProject", true).as_deref(),
            Some("\u{2325}\u{2318}N")
        );
        assert_eq!(
            default_shortcut_label_for_command("thread.copyReference", true).as_deref(),
            Some("\u{21e7}\u{2318}C")
        );
        assert_eq!(
            default_shortcut_label_for_command("thread.jump.3", true).as_deref(),
            Some("\u{2318}3")
        );
    }

    #[test]
    fn spells_modifiers_out_elsewhere() {
        assert_eq!(
            default_shortcut_label_for_command("projectSearch.toggle", false).as_deref(),
            Some("Ctrl+Shift+F")
        );
    }

    #[test]
    fn labels_named_keys() {
        assert_eq!(format_shortcut_key_label("escape"), "Esc");
        assert_eq!(format_shortcut_key_label("arrowup"), "Up");
        assert_eq!(format_shortcut_key_label("enter"), "Enter");
        assert_eq!(format_shortcut_key_label(" "), "Space");
        assert_eq!(
            default_shortcut_label_for_command("not.a.command", true),
            None
        );
    }
}
