//! Port of upstream/apps/web/src/components/settings/KeybindingsSettings.logic.ts
//! (and its test file).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use turin_contracts::keybindings::{KeybindingWhenNode, STATIC_KEYBINDING_COMMANDS};
use turin_shared::keybindings::{
    DEFAULT_RESOLVED_KEYBINDINGS, ParsedShortcut, ResolvedKeybindingRule, is_mac_platform,
    parse_keybinding_when_expression, shortcut_key_from_event,
};

use super::locale::locale_compare;
use super::usage_shortcuts::{METRIC_OPTIONS, WINDOW_OPTIONS};

/// Every usage.* command needs a rank. An unranked one falls back to the
/// alphabetical compare, which makes the comparator inconsistent and the order
/// depend on the input order.
static USAGE_COMMAND_ORDER: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    std::iter::once("usage.open")
        .chain(METRIC_OPTIONS.iter().map(|option| option.command))
        .chain(WINDOW_OPTIONS.iter().map(|option| option.command))
        .enumerate()
        .map(|(index, command)| (command, index))
        .collect()
});

fn first_usage_command() -> &'static str {
    METRIC_OPTIONS[0].command
}

/// Orders commands by `key`, except Usage page commands, which sort as one
/// block in page order where the first of them would sort. A total order, so
/// adding a binding elsewhere cannot reshuffle the Usage rows.
fn compare_commands(
    left: &str,
    right: &str,
    key: impl Fn(&str) -> String,
) -> std::cmp::Ordering {
    let left_rank = USAGE_COMMAND_ORDER.get(left);
    let right_rank = USAGE_COMMAND_ORDER.get(right);
    if let (Some(l), Some(r)) = (left_rank, right_rank) {
        return l.cmp(r);
    }
    locale_compare(
        &key(if left_rank.is_none() {
            left
        } else {
            first_usage_command()
        }),
        &key(if right_rank.is_none() {
            right
        } else {
            first_usage_command()
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingSource {
    Default,
    Custom,
    Project,
}

impl KeybindingSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Custom => "Custom",
            Self::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeybindingRow {
    pub id: String,
    pub command: String,
    pub key: String,
    pub when: String,
    pub source: KeybindingSource,
    pub default_key: Option<String>,
    pub default_when: String,
    pub binding: ResolvedKeybindingRule,
    pub conflicts: Vec<String>,
}

const CORE_WHEN_VARIABLES: [&str; 6] = [
    "terminalFocus",
    "terminalOpen",
    "isWeb",
    "isDesktop",
    "true",
    "false",
];

static DEFAULT_WHEN_VARIABLES: LazyLock<Vec<String>> = LazyLock::new(|| {
    // A Set in insertion order.
    let mut identifiers: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for core in CORE_WHEN_VARIABLES {
        if seen.insert(core.to_owned()) {
            identifiers.push(core.to_owned());
        }
    }
    for binding in DEFAULT_RESOLVED_KEYBINDINGS.iter() {
        collect_when_identifiers_from_node(binding.when_ast.as_ref(), &mut identifiers, &mut seen);
    }
    identifiers
});

pub fn default_when_variable() -> String {
    DEFAULT_WHEN_VARIABLES
        .iter()
        .find(|identifier| *identifier != "true" && *identifier != "false")
        .cloned()
        .unwrap_or_else(|| "terminalFocus".to_owned())
}

/// `shortcutToKeybindingInput`.
pub fn shortcut_to_keybinding_input(shortcut: &ParsedShortcut) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if shortcut.mod_key {
        parts.push("mod");
    }
    if shortcut.meta_key {
        parts.push("meta");
    }
    if shortcut.ctrl_key {
        parts.push("ctrl");
    }
    if shortcut.alt_key {
        parts.push("alt");
    }
    if shortcut.shift_key {
        parts.push("shift");
    }
    let key = shortcut.key.as_str();
    parts.push(match key {
        " " => "space",
        "escape" => "esc",
        other => other,
    });
    parts.join("+")
}

pub fn when_ast_to_expression(node: Option<&KeybindingWhenNode>) -> String {
    let Some(node) = node else {
        return String::new();
    };
    match node {
        KeybindingWhenNode::Identifier { name } => name.clone(),
        KeybindingWhenNode::Not { node } => format!("!{}", wrap_when_expression(node)),
        KeybindingWhenNode::And { left, right } => format!(
            "{} && {}",
            wrap_when_expression(left),
            wrap_when_expression(right)
        ),
        KeybindingWhenNode::Or { left, right } => format!(
            "{} || {}",
            wrap_when_expression(left),
            wrap_when_expression(right)
        ),
    }
}

pub fn when_node_remove_label(node: &KeybindingWhenNode, depth: usize) -> &'static str {
    if depth == 0 {
        return "Clear all conditions";
    }
    match node {
        KeybindingWhenNode::Identifier { .. } => "Remove condition",
        KeybindingWhenNode::Not { node: inner }
            if matches!(**inner, KeybindingWhenNode::Identifier { .. }) =>
        {
            "Remove condition"
        }
        _ => "Remove group and its conditions",
    }
}

fn wrap_when_expression(node: &KeybindingWhenNode) -> String {
    match node {
        KeybindingWhenNode::Identifier { .. } | KeybindingWhenNode::Not { .. } => {
            when_ast_to_expression(Some(node))
        }
        _ => format!("({})", when_ast_to_expression(Some(node))),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WhenExpressionDraft {
    Ok(Option<KeybindingWhenNode>),
    Err { message: String },
}

pub fn parse_when_expression_draft(expression: &str) -> WhenExpressionDraft {
    let trimmed = turin_contracts::base_schemas::js_trim(expression);
    if trimmed.is_empty() {
        return WhenExpressionDraft::Ok(None);
    }
    match parse_keybinding_when_expression(&trimmed) {
        Some(ast) => WhenExpressionDraft::Ok(Some(ast)),
        None => WhenExpressionDraft::Err {
            message: "Use variables with !, &&, ||, and parentheses.".to_owned(),
        },
    }
}

fn matches_binding(entry: &ResolvedKeybindingRule, binding: &ResolvedKeybindingRule) -> bool {
    entry.command == binding.command
        && shortcut_to_keybinding_input(&entry.shortcut)
            == shortcut_to_keybinding_input(&binding.shortcut)
        && when_ast_to_expression(entry.when_ast.as_ref())
            == when_ast_to_expression(binding.when_ast.as_ref())
}

fn source_for_binding(binding: &ResolvedKeybindingRule) -> KeybindingSource {
    if binding.command.as_str().starts_with("script.") {
        return KeybindingSource::Project;
    }
    if DEFAULT_RESOLVED_KEYBINDINGS
        .iter()
        .any(|entry| matches_binding(entry, binding))
    {
        KeybindingSource::Default
    } else {
        KeybindingSource::Custom
    }
}

fn default_binding_for_binding(
    binding: &ResolvedKeybindingRule,
) -> Option<&'static ResolvedKeybindingRule> {
    let binding_when = when_ast_to_expression(binding.when_ast.as_ref());
    DEFAULT_RESOLVED_KEYBINDINGS
        .iter()
        .find(|entry| matches_binding(entry, binding))
        .or_else(|| {
            DEFAULT_RESOLVED_KEYBINDINGS.iter().find(|entry| {
                entry.command == binding.command
                    && when_ast_to_expression(entry.when_ast.as_ref()) == binding_when
            })
        })
        .or_else(|| {
            DEFAULT_RESOLVED_KEYBINDINGS
                .iter()
                .find(|entry| entry.command == binding.command)
        })
}

fn keybinding_row_id(command: &str, key: &str, when: &str) -> String {
    format!("{command}\0{key}\0{when}")
}

fn conflicts_with_when(left: &str, right: &str) -> bool {
    left.is_empty() || right.is_empty() || left == right
}

pub fn keybinding_conflict_labels(
    rows: &[KeybindingRow],
    row_id: &str,
    key: &str,
    when: &str,
) -> Vec<String> {
    if turin_contracts::base_schemas::js_trim(key).is_empty() {
        return Vec::new();
    }
    let conflicts: BTreeSet<String> = rows
        .iter()
        .filter(|candidate| {
            candidate.id != row_id && candidate.key == key && conflicts_with_when(&candidate.when, when)
        })
        .map(|candidate| command_label(&candidate.command))
        .collect();
    // `toSorted()` default order: UTF-16 code units, which BTreeSet matches
    // for ASCII labels.
    conflicts.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeybindingGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub rows: Vec<KeybindingRow>,
}

struct GroupSpec {
    id: &'static str,
    title: &'static str,
    prefixes: &'static [&'static str],
}

/// Page sections in display order; a command joins the first group listing its prefix.
const KEYBINDING_GROUPS: [GroupSpec; 7] = [
    GroupSpec {
        id: "navigation",
        title: "Navigation",
        prefixes: &[
            "sidebar",
            "rightPanel",
            "commandPalette",
            "filePicker",
            "projectSearch",
            "editor",
        ],
    },
    GroupSpec {
        id: "threads",
        title: "Threads",
        prefixes: &["thread", "chat", "pullRequest"],
    },
    GroupSpec {
        id: "composer",
        title: "Composer",
        prefixes: &["composer", "modelPicker"],
    },
    GroupSpec {
        id: "terminal",
        title: "Terminal",
        prefixes: &["terminal"],
    },
    GroupSpec {
        id: "preview",
        title: "Preview & diff",
        prefixes: &["preview", "diff"],
    },
    GroupSpec {
        id: "appearance",
        title: "Appearance",
        prefixes: &["theme", "appearance", "themeEditor"],
    },
    GroupSpec {
        id: "scripts",
        title: "Project scripts",
        prefixes: &["script"],
    },
];
const OTHER_GROUP: (&str, &str) = ("other", "Other");

fn keybinding_group_for(command: &str) -> (&'static str, &'static str) {
    let prefix = command.split('.').next().unwrap_or("");
    KEYBINDING_GROUPS
        .iter()
        .find(|group| group.prefixes.contains(&prefix))
        .map(|group| (group.id, group.title))
        .unwrap_or(OTHER_GROUP)
}

/// Splits sorted rows into the page's sections, dropping sections with no rows.
pub fn group_keybinding_rows(rows: &[KeybindingRow]) -> Vec<KeybindingGroup> {
    let mut by_group: HashMap<&'static str, Vec<KeybindingRow>> = HashMap::new();
    for row in rows {
        by_group
            .entry(keybinding_group_for(&row.command).0)
            .or_default()
            .push(row.clone());
    }
    KEYBINDING_GROUPS
        .iter()
        .map(|group| (group.id, group.title))
        .chain(std::iter::once(OTHER_GROUP))
        .filter_map(|(id, title)| {
            by_group.remove(id).map(|rows| KeybindingGroup { id, title, rows })
        })
        .collect()
}

pub fn build_keybinding_rows(
    keybindings: &[ResolvedKeybindingRule],
    query: &str,
) -> Vec<KeybindingRow> {
    let normalized_query = turin_contracts::base_schemas::js_trim(query).to_lowercase();
    let rows: Vec<KeybindingRow> = keybindings
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            let default_binding = default_binding_for_binding(binding);
            let key = shortcut_to_keybinding_input(&binding.shortcut);
            let when = when_ast_to_expression(binding.when_ast.as_ref());
            KeybindingRow {
                id: format!(
                    "{}\0{index}",
                    keybinding_row_id(binding.command.as_str(), &key, &when)
                ),
                command: binding.command.as_str().to_owned(),
                key,
                when,
                source: source_for_binding(binding),
                default_key: default_binding
                    .map(|binding| shortcut_to_keybinding_input(&binding.shortcut)),
                default_when: when_ast_to_expression(
                    default_binding.and_then(|binding| binding.when_ast.as_ref()),
                ),
                binding: binding.clone(),
                conflicts: Vec::new(),
            }
        })
        .collect();

    let mut rows_with_conflicts: Vec<KeybindingRow> = rows
        .iter()
        .map(|row| {
            let conflicts = keybinding_conflict_labels(&rows, &row.id, &row.key, &row.when);
            let mut row = row.clone();
            row.conflicts = conflicts;
            row
        })
        .collect();

    rows_with_conflicts.sort_by(|left, right| {
        compare_commands(&left.command, &right.command, str::to_owned)
            .then_with(|| locale_compare(&left.key, &right.key))
    });

    if normalized_query.is_empty() {
        return rows_with_conflicts;
    }

    rows_with_conflicts
        .into_iter()
        .filter(|row| {
            row.command.to_lowercase().contains(&normalized_query)
                || command_label(&row.command)
                    .to_lowercase()
                    .contains(&normalized_query)
                || row.key.to_lowercase().contains(&normalized_query)
                || row.when.to_lowercase().contains(&normalized_query)
                || row.source.label().to_lowercase().contains(&normalized_query)
        })
        .collect()
}

fn collect_when_identifiers_from_node(
    node: Option<&KeybindingWhenNode>,
    identifiers: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let Some(node) = node else {
        return;
    };
    match node {
        KeybindingWhenNode::Identifier { name } => {
            if seen.insert(name.clone()) {
                identifiers.push(name.clone());
            }
        }
        KeybindingWhenNode::Not { node } => {
            collect_when_identifiers_from_node(Some(node), identifiers, seen)
        }
        KeybindingWhenNode::And { left, right } | KeybindingWhenNode::Or { left, right } => {
            collect_when_identifiers_from_node(Some(left), identifiers, seen);
            collect_when_identifiers_from_node(Some(right), identifiers, seen);
        }
    }
}

pub fn is_known_when_variable(identifier: &str) -> bool {
    DEFAULT_WHEN_VARIABLES.iter().any(|known| known == identifier)
}

pub fn unknown_when_variables(node: Option<&KeybindingWhenNode>) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut seen = HashSet::new();
    collect_when_identifiers_from_node(node, &mut identifiers, &mut seen);
    let mut unknown: Vec<String> = identifiers
        .into_iter()
        .filter(|identifier| !is_known_when_variable(identifier))
        .collect();
    unknown.sort();
    unknown
}

pub fn build_when_variable_options() -> Vec<String> {
    let mut options: Vec<String> = DEFAULT_WHEN_VARIABLES.clone();
    options.sort_by(|left, right| {
        let left_core = CORE_WHEN_VARIABLES.iter().position(|core| core == left);
        let right_core = CORE_WHEN_VARIABLES.iter().position(|core| core == right);
        if left_core.is_some() || right_core.is_some() {
            return left_core
                .unwrap_or(usize::MAX)
                .cmp(&right_core.unwrap_or(usize::MAX));
        }
        locale_compare(left, right)
    });
    options
}

pub fn build_keybinding_command_options(keybindings: &[ResolvedKeybindingRule]) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for command in STATIC_KEYBINDING_COMMANDS
        .iter()
        .map(|command| (*command).to_owned())
        .chain(keybindings.iter().map(|binding| binding.command.as_str().to_owned()))
    {
        if seen.insert(command.clone()) {
            commands.push(command);
        }
    }
    commands.sort_by(|left, right| compare_commands(left, right, command_label));
    commands
}

pub fn command_label(command: &str) -> String {
    match command {
        "composer.sendAlternate" => return "Composer: Opposite Queue or Steer Action".to_owned(),
        "composer.sendBackground" => return "Composer: Start in Background".to_owned(),
        "composer.sendAndNewThread" => {
            return "Composer: Send and Start New Thread".to_owned();
        }
        "thread.steerQueuedMessage" => {
            return "Queue: Send First Queued Message as Steer".to_owned();
        }
        "thread.editQueuedMessage" => return "Queue: Edit Last Queued Message".to_owned(),
        "thread.copyReference" => return "Pull Request: Copy Link or Thread ID".to_owned(),
        _ => {}
    }
    if let Some(metric) = METRIC_OPTIONS.iter().find(|option| option.command == command) {
        return format!("Usage: {}", metric.label);
    }
    if let Some(period) = WINDOW_OPTIONS.iter().find(|option| option.command == command) {
        return format!("Usage: Period: {}", period.label);
    }
    match command {
        "view.reopenClosed" => return "Reopen Closed Tab".to_owned(),
        "preview.focusUrl" => return "Preview: Focus URL".to_owned(),
        _ => {}
    }
    if let Some(inner) = command
        .strip_prefix("script.")
        .and_then(|rest| rest.strip_suffix(".run"))
    {
        return format!("Run Script: {}", title_case_command_segment(inner));
    }
    command
        .split('.')
        .map(title_case_command_segment)
        .collect::<Vec<_>>()
        .join(": ")
}

fn title_case_command_segment(segment: &str) -> String {
    // `replace(/([a-z0-9])([A-Z])/g, "$1 $2")`
    let mut spaced = String::with_capacity(segment.len() + 4);
    let mut previous: Option<char> = None;
    for c in segment.chars() {
        if let Some(p) = previous
            && (p.is_ascii_lowercase() || p.is_ascii_digit())
            && c.is_ascii_uppercase()
        {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_shortcut_key_token(key: &str) -> Option<String> {
    let normalized = key.to_lowercase();
    if matches!(
        normalized.as_str(),
        "meta" | "control" | "ctrl" | "shift" | "alt" | "option"
    ) {
        return None;
    }
    if normalized == " " {
        return Some("space".to_owned());
    }
    if normalized == "escape" {
        return Some("esc".to_owned());
    }
    if matches!(
        normalized.as_str(),
        "arrowup" | "arrowdown" | "arrowleft" | "arrowright"
    ) {
        return Some(normalized);
    }
    if normalized.chars().count() == 1 {
        return Some(normalized);
    }
    // /^f\d{1,2}$/
    if let Some(digits) = normalized.strip_prefix('f')
        && (1..=2).contains(&digits.len())
        && digits.chars().all(|c| c.is_ascii_digit())
    {
        return Some(normalized);
    }
    if matches!(
        normalized.as_str(),
        "enter" | "tab" | "backspace" | "delete" | "home" | "end" | "pageup" | "pagedown"
    ) {
        return Some(normalized);
    }
    None
}

/// The fields of a keydown that [`keybinding_from_keyboard_event`] reads.
#[derive(Debug, Clone, Default)]
pub struct KeyboardEvent {
    pub key: String,
    pub code: String,
    pub meta_key: bool,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
}

/// Turns a keydown into a binding such as `mod+shift+k` or `tab`. `None` for modifier-only presses.
pub fn keybinding_from_keyboard_event(event: &KeyboardEvent, platform: &str) -> Option<String> {
    let key_token = normalize_shortcut_key_token(&shortcut_key_from_event(
        &event.key,
        Some(event.code.as_str()),
    ))?;

    let mut parts: Vec<String> = Vec::new();
    if is_mac_platform(platform) {
        if event.meta_key {
            parts.push("mod".into());
        }
        if event.ctrl_key {
            parts.push("ctrl".into());
        }
    } else {
        if event.ctrl_key {
            parts.push("mod".into());
        }
        if event.meta_key {
            parts.push("meta".into());
        }
    }
    if event.alt_key {
        parts.push("alt".into());
    }
    if event.shift_key {
        parts.push("shift".into());
    }
    parts.push(key_token);
    Some(parts.join("+"))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use turin_contracts::keybindings::KeybindingCommand;
    use turin_shared::keybindings::parse_keybinding_shortcut;

    use super::*;

    fn rule(command: &str, key: &str, when: Option<KeybindingWhenNode>) -> ResolvedKeybindingRule {
        ResolvedKeybindingRule {
            command: KeybindingCommand::new(command).unwrap(),
            shortcut: parse_keybinding_shortcut(key).unwrap(),
            when_ast: when,
        }
    }

    fn ident(name: &str) -> KeybindingWhenNode {
        KeybindingWhenNode::Identifier { name: name.into() }
    }

    fn not(node: KeybindingWhenNode) -> KeybindingWhenNode {
        KeybindingWhenNode::Not { node: Box::new(node) }
    }

    fn commands(rows: &[KeybindingRow]) -> Vec<&str> {
        rows.iter().map(|row| row.command.as_str()).collect()
    }

    #[test]
    fn lists_composer_provider_and_pull_request_commands_with_editable_defaults() {
        let rows = build_keybinding_rows(&DEFAULT_RESOLVED_KEYBINDINGS, "");
        for command in [
            "composer.sendAlternate",
            "composer.sendBackground",
            "thread.steerQueuedMessage",
            "thread.editQueuedMessage",
            "composer.host",
            "composer.effort",
            "composer.mode",
            "composer.workspace",
            "composer.branch",
            "composer.previousWorktree",
            "modelPicker.previousProvider",
            "modelPicker.nextProvider",
            "thread.copyReference",
            "pullRequest.copyNumber",
        ] {
            let row = rows
                .iter()
                .find(|row| row.command == command)
                .unwrap_or_else(|| panic!("{command} has a row"));
            assert_eq!(row.source, KeybindingSource::Default, "{command}");
            assert!(row.conflicts.is_empty(), "{command}");
        }
    }

    #[test]
    fn finds_the_editable_shortcut_for_sending_the_first_queued_message() {
        let rows = build_keybinding_rows(&DEFAULT_RESOLVED_KEYBINDINGS, "first queued");
        assert!(rows.iter().any(|row| row.command == "thread.steerQueuedMessage"
            && row.key == "mod+shift+enter"));
    }

    #[test]
    fn finds_the_copy_link_shortcut() {
        for query in ["pu", "pull request", "copy link", "thread id"] {
            let rows = build_keybinding_rows(&DEFAULT_RESOLVED_KEYBINDINGS, query);
            assert!(
                rows.iter()
                    .any(|row| row.command == "thread.copyReference" && row.key == "mod+shift+c"),
                "{query}"
            );
        }
    }

    #[test]
    fn groups_rows_by_command_area_in_page_order_and_drops_empty_groups() {
        let groups =
            group_keybinding_rows(&build_keybinding_rows(&DEFAULT_RESOLVED_KEYBINDINGS, ""));
        assert_eq!(
            groups.iter().map(|group| group.title).collect::<Vec<_>>(),
            vec![
                "Navigation",
                "Threads",
                "Composer",
                "Terminal",
                "Preview & diff",
                "Appearance",
                "Other"
            ]
        );
        let composer = groups.iter().find(|group| group.id == "composer").unwrap();
        let composer_commands = commands(&composer.rows);
        assert!(composer_commands.contains(&"composer.host"));
        assert!(composer_commands.contains(&"modelPicker.toggle"));
        let split = group_keybinding_rows(&build_keybinding_rows(&DEFAULT_RESOLVED_KEYBINDINGS, "split"));
        assert_eq!(split.len(), 1);
        assert_eq!(split[0].id, "terminal");
    }

    #[test]
    fn orders_usage_bindings_and_command_choices_like_the_page() {
        let expected = vec![
            "usage.open",
            "usage.cost",
            "usage.tokens",
            "usage.limits",
            "usage.period.day",
            "usage.period.week",
            "usage.period.month",
            "usage.period.quarter",
        ];
        let forward: Vec<ResolvedKeybindingRule> = DEFAULT_RESOLVED_KEYBINDINGS.clone();
        let reversed: Vec<ResolvedKeybindingRule> =
            DEFAULT_RESOLVED_KEYBINDINGS.iter().rev().cloned().collect();
        for bindings in [forward, reversed] {
            assert_eq!(
                commands(&build_keybinding_rows(&bindings, "usage")),
                expected
            );
            let options = build_keybinding_command_options(&bindings);
            assert_eq!(
                options
                    .iter()
                    .filter(|command| command.starts_with("usage."))
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[test]
    fn builds_searchable_rows_with_readable_key_and_when_values() {
        let rows = build_keybinding_rows(
            &[rule("terminal.toggle", "mod+j", Some(not(ident("terminalFocus"))))],
            "terminal",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].command, "terminal.toggle");
        assert_eq!(rows[0].key, "mod+j");
        assert_eq!(rows[0].when, "!terminalFocus");
        assert_eq!(rows[0].default_key.as_deref(), Some("mod+j"));
        assert_eq!(rows[0].default_when, "");
        assert_eq!(rows[0].source, KeybindingSource::Custom);
    }

    fn event(key: &str, code: &str, meta: bool, ctrl: bool, shift: bool) -> KeyboardEvent {
        KeyboardEvent {
            key: key.into(),
            code: code.into(),
            meta_key: meta,
            ctrl_key: ctrl,
            alt_key: false,
            shift_key: shift,
        }
    }

    #[test]
    fn captures_platform_specific_mod_shortcuts() {
        assert_eq!(
            keybinding_from_keyboard_event(&event("K", "KeyK", true, false, true), "MacIntel"),
            Some("mod+shift+k".into())
        );
        assert_eq!(
            keybinding_from_keyboard_event(&event("K", "KeyK", false, true, true), "Win32"),
            Some("mod+shift+k".into())
        );
    }

    #[test]
    fn captures_keys_without_a_modifier() {
        for (key, code, expected) in [("k", "KeyK", "k"), ("Tab", "Tab", "tab"), ("F5", "F5", "f5")] {
            assert_eq!(
                keybinding_from_keyboard_event(&event(key, code, false, false, false), "MacIntel"),
                Some(expected.into())
            );
        }
    }

    #[test]
    fn waits_for_a_key_when_only_a_modifier_is_pressed() {
        assert_eq!(
            keybinding_from_keyboard_event(&event("Meta", "MetaLeft", true, false, false), "MacIntel"),
            None
        );
    }

    #[test]
    fn captures_shifted_punctuation_by_physical_key() {
        for (key, code, expected) in [
            ("@", "Digit2", "mod+shift+2"),
            ("\"", "Digit2", "mod+shift+2"),
            ("@", "Quote", "mod+shift+'"),
        ] {
            assert_eq!(
                keybinding_from_keyboard_event(&event(key, code, true, false, true), "MacIntel"),
                Some(expected.into()),
                "{key} at {code}"
            );
        }
    }

    #[test]
    fn captures_latin_layout_keys_instead_of_their_punctuation_position() {
        assert_eq!(
            keybinding_from_keyboard_event(&event("m", "Semicolon", true, false, false), "MacIntel"),
            Some("mod+m".into())
        );
    }

    #[test]
    fn serializes_shortcuts_and_when_expressions_for_upserts() {
        let space = ParsedShortcut {
            key: parse_keybinding_shortcut("mod+alt+space").unwrap().key,
            mod_key: true,
            meta_key: false,
            ctrl_key: false,
            alt_key: true,
            shift_key: false,
        };
        assert_eq!(shortcut_to_keybinding_input(&space), "mod+alt+space");

        let and = KeybindingWhenNode::And {
            left: Box::new(ident("editorFocus")),
            right: Box::new(not(ident("terminalFocus"))),
        };
        assert_eq!(
            when_ast_to_expression(Some(&and)),
            "editorFocus && !terminalFocus"
        );

        let nested = KeybindingWhenNode::And {
            left: Box::new(ident("editorFocus")),
            right: Box::new(KeybindingWhenNode::Or {
                left: Box::new(not(ident("terminalFocus"))),
                right: Box::new(ident("modelPickerOpen")),
            }),
        };
        assert_eq!(
            parse_when_expression_draft("editorFocus && (!terminalFocus || modelPickerOpen)"),
            WhenExpressionDraft::Ok(Some(nested))
        );
        assert_eq!(
            parse_when_expression_draft("editorFocus &&"),
            WhenExpressionDraft::Err {
                message: "Use variables with !, &&, ||, and parentheses.".into()
            }
        );
        let negated = not(KeybindingWhenNode::Or {
            left: Box::new(ident("terminalFocus")),
            right: Box::new(ident("modelPickerOpen")),
        });
        assert_eq!(
            parse_when_expression_draft("!(terminalFocus || modelPickerOpen)"),
            WhenExpressionDraft::Ok(Some(negated))
        );
    }

    #[test]
    fn describes_the_scope_of_each_visual_expression_removal() {
        let condition = ident("terminalFocus");
        let negated_condition = not(condition.clone());
        let group = KeybindingWhenNode::And {
            left: Box::new(condition.clone()),
            right: Box::new(negated_condition.clone()),
        };
        let negated_group = not(group.clone());

        assert_eq!(when_node_remove_label(&group, 0), "Clear all conditions");
        assert_eq!(when_node_remove_label(&condition, 1), "Remove condition");
        assert_eq!(when_node_remove_label(&negated_condition, 1), "Remove condition");
        assert_eq!(
            when_node_remove_label(&group, 1),
            "Remove group and its conditions"
        );
        assert_eq!(
            when_node_remove_label(&negated_group, 1),
            "Remove group and its conditions"
        );
    }

    #[test]
    fn formats_static_and_project_script_command_labels() {
        assert_eq!(command_label("commandPalette.toggle"), "Command Palette: Toggle");
        assert_eq!(command_label("themeEditor.toggle"), "Theme Editor: Toggle");
        assert_eq!(command_label("view.reopenClosed"), "Reopen Closed Tab");
        assert_eq!(command_label("preview.focusUrl"), "Preview: Focus URL");
        assert_eq!(command_label("script.setup-db.run"), "Run Script: Setup Db");
    }

    #[test]
    fn builds_known_when_variable_options_from_defaults_without_frontend_labels() {
        let options = build_when_variable_options();
        for expected in [
            "terminalFocus",
            "terminalOpen",
            "isWeb",
            "isDesktop",
            "modelPickerOpen",
            "true",
            "false",
        ] {
            assert!(options.iter().any(|option| option == expected), "{expected}");
        }
        assert!(!options.iter().any(|option| option == "customModeActive"));
    }

    #[test]
    fn builds_command_options_from_all_static_commands_and_resolved_project_bindings() {
        let options =
            build_keybinding_command_options(&[rule("script.setup-db.run", "mod+r", None)]);
        for expected in [
            "chat.new",
            "threadPanel.toggle",
            "rightPanel.toggleMaximized",
            "composer.cycleHost",
            "thread.stop",
            "usage.open",
            "script.setup-db.run",
        ] {
            assert!(options.iter().any(|option| option == expected), "{expected}");
        }
        for command in ["thread.stop", "composer.cycleHost"] {
            assert!(
                !DEFAULT_RESOLVED_KEYBINDINGS
                    .iter()
                    .any(|binding| binding.command.as_str() == command),
                "{command}"
            );
        }
    }

    #[test]
    fn reports_unknown_when_variables_without_rejecting_parseable_expressions() {
        let WhenExpressionDraft::Ok(value) =
            parse_when_expression_draft("!terminalFocus && terminalFoc")
        else {
            panic!("parses");
        };
        assert_eq!(unknown_when_variables(value.as_ref()), vec!["terminalFoc"]);
    }

    #[test]
    fn marks_each_default_shortcut_for_multi_binding_commands_as_default() {
        let when = || Some(not(ident("terminalFocus")));
        let rows = build_keybinding_rows(
            &[
                rule("chat.new", "mod+n", when()),
                rule("chat.new", "mod+shift+o", when()),
            ],
            "",
        );
        assert_eq!(
            rows.iter().map(|row| row.source).collect::<Vec<_>>(),
            vec![KeybindingSource::Default, KeybindingSource::Default]
        );
    }

    #[test]
    fn reports_conflicting_shortcuts_that_share_an_active_when_context() {
        let when = || Some(not(ident("terminalFocus")));
        let rows = build_keybinding_rows(
            &[
                rule("chat.new", "mod+n", when()),
                rule("chat.newLocal", "mod+n", when()),
            ],
            "",
        );
        assert_eq!(rows[0].conflicts, vec!["Chat: New Local".to_owned()]);
        assert_eq!(
            keybinding_conflict_labels(&rows, &rows[0].id, "mod+n", ""),
            vec!["Chat: New Local".to_owned()]
        );
    }
}
