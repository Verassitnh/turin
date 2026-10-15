//! Port of upstream/packages/shared/src/editor.ts (`resolveEditorCommand`),
//! macOS branch only. The Windows and Linux install locations are cut.
//!
//! T05b owns this upstream file. It lives here until turin-shared has it, so
//! both launcher callers share one copy. See PORTING.md Deviations.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::editor::{EditorDefinition, EditorId, EditorLaunchStyle};

use crate::host::{Env, LaunchHost};

/// Resolved executable plus the base args to pass before the launcher's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    pub command: String,
    pub base_args: Vec<String>,
}

/// Bundle names that differ from the editor's label, as upstream `installNames`.
fn install_names(id: EditorId) -> &'static [&'static str] {
    match id {
        EditorId::Vscode => &["Visual Studio Code"],
        EditorId::VscodeInsiders => &["Visual Studio Code - Insiders"],
        // `Antigravity.app` is the separate Hub, not the IDE.
        EditorId::Antigravity => &["Antigravity IDE"],
        EditorId::Idea => &[
            "IntelliJ IDEA",
            "IntelliJ IDEA CE",
            "IntelliJ IDEA Ultimate",
        ],
        EditorId::Pycharm => &["PyCharm", "PyCharm CE"],
        EditorId::Rider => &["Rider", "JetBrains Rider"],
        _ => &[],
    }
}

static VERSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9][0-9.]*$").expect("static pattern"));

/// True for `<name> <version>.app`, where the version is digits and dots
/// (`IntelliJ IDEA 2026.1.4.app`). A plain prefix match would let one editor
/// steal another's bundle, e.g. stable VS Code resolving to
/// `Visual Studio Code - Insiders.app`.
fn is_versioned_bundle(entry: &str, name: &str) -> bool {
    let Some(rest) = entry
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
    else {
        return false;
    };
    let Some(version) = rest.strip_suffix(".app") else {
        return false;
    };
    VERSION_PATTERN.is_match(version)
}

/// Upstream `resolveEditorCommand(editor, env)` on darwin: PATH first, then
/// the app bundles under `host.application_roots`, then the JetBrains
/// Toolbox script directory.
pub fn resolve_editor_command(
    editor: &EditorDefinition,
    host: &dyn LaunchHost,
    env: &Env,
) -> Option<EditorCommand> {
    let commands = editor.commands?;
    let base_args: Vec<String> = editor
        .base_args
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    // OpenZFS also installs a `zed` executable. Prefer the macOS editor bundle
    // so a PATH entry for the ZFS Event Daemon cannot take over editor opens.
    let prefer_bundle = editor.id == EditorId::Zed;
    if !prefer_bundle {
        for command in commands {
            if host.is_command_available(command, env) {
                return Some(EditorCommand {
                    command: (*command).to_owned(),
                    base_args,
                });
            }
        }
    }

    let home = env.get("HOME").filter(|home| !home.is_empty());
    let label = editor.label;
    let names: Vec<&str> = match install_names(editor.id) {
        [] => vec![label],
        names => names.to_vec(),
    };
    let command = commands[0];
    let jetbrains = editor.launch_style == EditorLaunchStyle::LineColumn;
    let mut candidates: Vec<String> = Vec::new();

    for root_path in host.application_roots(env) {
        let entries = host.read_directory(&root_path);
        // JetBrains Toolbox installs bundles named after the app and its
        // version (`IntelliJ IDEA 2026.1.4.app`), so list the directory and
        // accept `<name> <version>.app` as well as the exact `<name>.app`.
        let mut bundles: Vec<String> = names.iter().map(|name| format!("{name}.app")).collect();
        for entry in &entries {
            let matches = names
                .iter()
                .any(|name| entry == &format!("{name}.app") || is_versioned_bundle(entry, name));
            if matches && !bundles.contains(entry) {
                bundles.push(entry.clone());
            }
        }
        for bundle in bundles {
            let contents = root_path.join(&bundle).join("Contents");
            if jetbrains || editor.id == EditorId::Zed {
                let binary = if editor.id == EditorId::Zed {
                    "cli"
                } else {
                    command
                };
                candidates.push(contents.join("MacOS").join(binary).display().to_string());
            } else {
                candidates.push(
                    contents
                        .join("Resources/app/bin")
                        .join(command)
                        .display()
                        .to_string(),
                );
                candidates.push(
                    contents
                        .join("Resources/app/bin/code")
                        .display()
                        .to_string(),
                );
            }
        }
    }
    if let (Some(home), true) = (home, jetbrains) {
        candidates.push(
            Path::new(home)
                .join("Library/Application Support/JetBrains/Toolbox/scripts")
                .join(command)
                .display()
                .to_string(),
        );
    }
    if prefer_bundle {
        candidates.extend(commands.iter().map(|command| (*command).to_owned()));
    }

    candidates.into_iter().find_map(|candidate| {
        host.is_command_available(&candidate, env)
            .then(|| EditorCommand {
                command: candidate,
                // Kiro's bundle launcher takes no base args on macOS.
                base_args: if editor.id == EditorId::Kiro {
                    Vec::new()
                } else {
                    base_args.clone()
                },
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_bundle_matches_name_boundary_only() {
        assert!(is_versioned_bundle(
            "IntelliJ IDEA 2026.1.4.app",
            "IntelliJ IDEA"
        ));
        assert!(!is_versioned_bundle(
            "Visual Studio Code - Insiders.app",
            "Visual Studio Code"
        ));
        assert!(!is_versioned_bundle("WebStormHelper.app", "WebStorm"));
        assert!(!is_versioned_bundle("WebStorm Beta.app", "WebStorm"));
        assert!(!is_versioned_bundle("WebStorm 2026.2", "WebStorm"));
    }
}
