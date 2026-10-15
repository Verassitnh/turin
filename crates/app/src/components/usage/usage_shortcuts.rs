//! Port of upstream/apps/web/src/components/usage/usageShortcuts.ts
//!
//! Upstream resolves a DOM `keydown` against the server's keybindings. GPUI
//! delivers keys through key contexts and actions, so the page binds
//! `usage.*` actions in its own context (see `usage_page::bind_keys`); this
//! module keeps the option tables and the pure resolution (`when` clauses,
//! shortcut matching, the typing-target guard) that the keybindings screen and
//! tests share.

use std::collections::HashMap;

use turin_contracts::keybindings::KeybindingWhenNode;
use turin_shared::keybindings::{
    ResolvedKeybindingRule, ShortcutEvent, is_mac_platform, matches_keybinding_shortcut,
};

use super::usage_page_preferences::UsagePageMetric;

pub struct MetricOption {
    pub value: UsagePageMetric,
    pub label: &'static str,
    pub command: &'static str,
}

pub struct WindowOption {
    pub days: u32,
    pub label: &'static str,
    pub command: &'static str,
}

pub const METRIC_OPTIONS: [MetricOption; 3] = [
    MetricOption {
        value: UsagePageMetric::Cost,
        label: "Cost",
        command: "usage.cost",
    },
    MetricOption {
        value: UsagePageMetric::Tokens,
        label: "Tokens",
        command: "usage.tokens",
    },
    MetricOption {
        value: UsagePageMetric::Limits,
        label: "Limits",
        command: "usage.limits",
    },
];

pub const WINDOW_OPTIONS: [WindowOption; 4] = [
    WindowOption {
        days: 1,
        label: "Past 24h",
        command: "usage.period.day",
    },
    WindowOption {
        days: 7,
        label: "7 days",
        command: "usage.period.week",
    },
    WindowOption {
        days: 30,
        label: "30 days",
        command: "usage.period.month",
    },
    WindowOption {
        days: 90,
        label: "90 days",
        command: "usage.period.quarter",
    },
];

/// `ShortcutMatchContext`: the booleans `when` clauses read.
#[derive(Clone, Debug)]
pub struct ShortcutMatchContext(HashMap<String, bool>);

impl ShortcutMatchContext {
    /// The defaults `resolveContext` seeds, with `overrides` on top.
    pub fn with(overrides: &[(&str, bool)]) -> Self {
        let mut context: HashMap<String, bool> = [
            "terminalFocus",
            "terminalOpen",
            "previewFocus",
            "previewOpen",
            "isWeb",
            "editableFocus",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), false))
        .collect();
        // Turin is a desktop app.
        context.insert("isDesktop".to_owned(), true);
        context.extend(
            overrides
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value)),
        );
        Self(context)
    }

    fn get(&self, name: &str) -> bool {
        self.0.get(name).copied().unwrap_or(false)
    }
}

fn evaluate_when_node(node: &KeybindingWhenNode, context: &ShortcutMatchContext) -> bool {
    match node {
        KeybindingWhenNode::Identifier { name } => match name.as_str() {
            "true" => true,
            "false" => false,
            name => context.get(name),
        },
        KeybindingWhenNode::Not { node } => !evaluate_when_node(node, context),
        KeybindingWhenNode::And { left, right } => {
            evaluate_when_node(left, context) && evaluate_when_node(right, context)
        }
        KeybindingWhenNode::Or { left, right } => {
            evaluate_when_node(left, context) || evaluate_when_node(right, context)
        }
    }
}

fn matches_when_clause(
    when_ast: Option<&KeybindingWhenNode>,
    context: &ShortcutMatchContext,
) -> bool {
    when_ast.is_none_or(|node| evaluate_when_node(node, context))
}

/// `resolveShortcutCommand`: the last matching rule wins.
pub fn resolve_shortcut_command<'a>(
    event: &ShortcutEvent,
    keybindings: &'a [ResolvedKeybindingRule],
    platform: &str,
    context: &ShortcutMatchContext,
) -> Option<&'a str> {
    keybindings.iter().rev().find_map(|binding| {
        (matches_when_clause(binding.when_ast.as_ref(), context)
            && matches_keybinding_shortcut(event, &binding.shortcut, platform))
        .then(|| binding.command.as_str())
    })
}

/// `shortcutConflictKey` / `formatShortcutLabel` for the Usage tooltips.
fn format_shortcut_key_label(key: &str) -> String {
    match key {
        " " => "Space".to_owned(),
        "escape" => "Esc".to_owned(),
        "arrowup" => "Up".to_owned(),
        "arrowdown" => "Down".to_owned(),
        "arrowleft" => "Left".to_owned(),
        "arrowright" => "Right".to_owned(),
        key if key.chars().count() == 1 => key.to_uppercase(),
        key => {
            let mut chars = key.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

pub fn format_shortcut_label(
    shortcut: &turin_shared::keybindings::ParsedShortcut,
    platform: &str,
) -> String {
    let key_label = format_shortcut_key_label(shortcut.key.as_str());
    let use_meta_for_mod = is_mac_platform(platform);
    let show_meta = shortcut.meta_key || (shortcut.mod_key && use_meta_for_mod);
    let show_ctrl = shortcut.ctrl_key || (shortcut.mod_key && !use_meta_for_mod);
    if use_meta_for_mod {
        return format!(
            "{}{}{}{}{key_label}",
            if show_ctrl { "\u{2303}" } else { "" },
            if shortcut.alt_key { "\u{2325}" } else { "" },
            if shortcut.shift_key { "\u{21e7}" } else { "" },
            if show_meta { "\u{2318}" } else { "" },
        );
    }
    let mut parts = Vec::new();
    if show_ctrl {
        parts.push("Ctrl".to_owned());
    }
    if shortcut.alt_key {
        parts.push("Alt".to_owned());
    }
    if shortcut.shift_key {
        parts.push("Shift".to_owned());
    }
    if show_meta {
        parts.push("Meta".to_owned());
    }
    parts.push(key_label);
    parts.join("+")
}

fn shortcut_conflict_key(
    shortcut: &turin_shared::keybindings::ParsedShortcut,
    platform: &str,
) -> String {
    let use_meta_for_mod = is_mac_platform(platform);
    let meta = shortcut.meta_key || (shortcut.mod_key && use_meta_for_mod);
    let ctrl = shortcut.ctrl_key || (shortcut.mod_key && !use_meta_for_mod);
    [
        shortcut.key.as_str(),
        if meta { "meta" } else { "" },
        if ctrl { "ctrl" } else { "" },
        if shortcut.shift_key { "shift" } else { "" },
        if shortcut.alt_key { "alt" } else { "" },
    ]
    .join("|")
}

/// `shortcutLabelForCommand`: the label of the command's highest-priority
/// effective shortcut, or `None`.
pub fn shortcut_label_for_command(
    keybindings: &[ResolvedKeybindingRule],
    command: &str,
    platform: &str,
    context: &ShortcutMatchContext,
) -> Option<String> {
    let mut claimed = std::collections::HashSet::new();
    let mut first = None;
    for binding in keybindings.iter().rev() {
        if !matches_when_clause(binding.when_ast.as_ref(), context) {
            continue;
        }
        if !claimed.insert(shortcut_conflict_key(&binding.shortcut, platform)) {
            continue;
        }
        if binding.command.as_str() == command && first.is_none() {
            first = Some(format_shortcut_label(&binding.shortcut, platform));
        }
    }
    first
}

/// What had focus when the key went down (the DOM `event.target`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageShortcutTarget {
    pub tag_name: String,
    pub is_content_editable: bool,
    /// `target.closest('[role="dialog"], [aria-modal="true"], [data-slot$="popup"]')`.
    pub inside_popup: bool,
}

/// Resolves page shortcuts without taking letters from fields or popup controls.
pub fn resolve_usage_shortcut<'a>(
    event: &ShortcutEvent,
    target: Option<&UsageShortcutTarget>,
    keybindings: &'a [ResolvedKeybindingRule],
    platform: &str,
) -> Option<&'a str> {
    if let Some(target) = target
        && (matches!(target.tag_name.as_str(), "INPUT" | "TEXTAREA" | "SELECT")
            || target.is_content_editable
            || target.inside_popup)
    {
        return None;
    }
    resolve_shortcut_command(
        event,
        keybindings,
        platform,
        &ShortcutMatchContext::with(&[("usagePageOpen", true)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_shared::keybindings::{
        compile_resolved_keybindings_config, default_keybinding_rules,
    };

    fn defaults() -> Vec<ResolvedKeybindingRule> {
        compile_resolved_keybindings_config(&default_keybinding_rules())
    }

    fn key_event(key: &str) -> ShortcutEvent {
        ShortcutEvent {
            key: key.to_owned(),
            ..Default::default()
        }
    }

    fn target(tag: &str, editable: bool) -> UsageShortcutTarget {
        UsageShortcutTarget {
            tag_name: tag.to_owned(),
            is_content_editable: editable,
            inside_popup: false,
        }
    }

    #[test]
    fn leaves_letters_to_the_focused_field() {
        let keybindings = defaults();
        for field in [
            target("INPUT", false),
            target("TEXTAREA", false),
            target("DIV", true),
        ] {
            assert_eq!(
                resolve_usage_shortcut(&key_event("t"), Some(&field), &keybindings, "Linux"),
                None,
                "{}",
                field.tag_name
            );
            assert_eq!(
                resolve_usage_shortcut(
                    &key_event("t"),
                    Some(&target("BODY", false)),
                    &keybindings,
                    "Linux"
                ),
                Some("usage.tokens")
            );
        }
    }

    #[test]
    fn popups_and_dialogs_keep_their_keys() {
        let keybindings = defaults();
        let popup = UsageShortcutTarget {
            tag_name: "BUTTON".to_owned(),
            is_content_editable: false,
            inside_popup: true,
        };
        assert_eq!(
            resolve_usage_shortcut(&key_event("c"), Some(&popup), &keybindings, "Linux"),
            None
        );
    }

    #[test]
    fn usage_commands_resolve_only_with_the_usage_page_open() {
        let keybindings = defaults();
        let closed = ShortcutMatchContext::with(&[]);
        assert_eq!(
            resolve_shortcut_command(&key_event("l"), &keybindings, "Linux", &closed),
            None
        );
        assert_eq!(
            resolve_usage_shortcut(&key_event("l"), None, &keybindings, "Linux"),
            Some("usage.limits")
        );
    }

    #[test]
    fn shift_chords_pick_the_period() {
        let keybindings = defaults();
        let event = ShortcutEvent {
            key: "1".to_owned(),
            ctrl_key: true,
            shift_key: true,
            ..Default::default()
        };
        assert_eq!(
            resolve_usage_shortcut(&event, None, &keybindings, "Linux"),
            Some("usage.period.day")
        );
    }

    #[test]
    fn tooltip_labels_follow_the_platform() {
        let keybindings = defaults();
        let usage = ShortcutMatchContext::with(&[("usagePageOpen", true)]);
        assert_eq!(
            shortcut_label_for_command(&keybindings, "usage.cost", "MacIntel", &usage),
            Some("C".to_owned())
        );
        assert_eq!(
            shortcut_label_for_command(&keybindings, "usage.period.day", "MacIntel", &usage),
            Some("\u{21e7}\u{2318}1".to_owned())
        );
        assert_eq!(
            shortcut_label_for_command(&keybindings, "usage.period.day", "Linux", &usage),
            Some("Ctrl+Shift+1".to_owned())
        );
        assert_eq!(
            shortcut_label_for_command(
                &keybindings,
                "usage.cost",
                "MacIntel",
                &ShortcutMatchContext::with(&[])
            ),
            None
        );
    }
}
