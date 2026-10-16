//! Text rules that upstream expresses with JS `trim()` and `/\s/`. Rust's
//! `str::trim` differs at the edges: JS strips U+FEFF but not U+0085.

use super::message_dispatch::is_native_maintenance_command;
use super::message_steer::is_goal_command;

#[test]
fn native_commands_ignore_a_byte_order_mark_but_not_a_next_line_character() {
    assert!(is_native_maintenance_command(
        "\u{feff}/compact\u{feff}",
        &[]
    ));
    assert!(is_native_maintenance_command("  /LOGOUT\n", &[]));
    assert!(!is_native_maintenance_command("\u{85}/compact", &[]));
}

#[test]
fn a_goal_command_ends_at_js_whitespace_only() {
    assert!(is_goal_command("/goal", &[]));
    assert!(is_goal_command("\u{feff}/goal\u{a0}ship it", &[]));
    assert!(!is_goal_command("/goal\u{85}ship it", &[]));
    assert!(!is_goal_command("/goals", &[]));
}
