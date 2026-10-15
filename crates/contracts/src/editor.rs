//! Port of upstream/packages/contracts/src/editor.ts

use crate::base_schemas::TrimmedNonEmptyString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditorLaunchStyle {
    DirectPath,
    Goto,
    LineColumn,
}

/// One known editor the external launcher can drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorDefinition {
    pub id: EditorId,
    pub label: &'static str,
    /// `None` for the file manager, which has no command to run.
    pub commands: Option<&'static [&'static str]>,
    pub base_args: &'static [&'static str],
    pub launch_style: EditorLaunchStyle,
    /// URL scheme for editors that support VS Code's remote deep links
    /// (`<scheme>://vscode-remote/ssh-remote+<host><path>`). Only set for VS
    /// Code and forks that ship the Remote-SSH machinery, plus Zed, which
    /// uses its own `zed://ssh/<host><path>` shape.
    pub remote_scheme: Option<&'static str>,
    /// JetBrains product code (`IU`, `PY`, ...). JetBrains IDEs open remote
    /// projects through the Toolbox App's `jetbrains://gateway/ssh/...`
    /// link, which uses the code to choose the IDE backend.
    pub jetbrains_product_code: Option<&'static str>,
}

macro_rules! editor {
    (
        $variant:ident = $label:literal, [$($cmd:literal),*], [$($base:literal),*],
        $style:ident, $remote:expr, $jetbrains:expr
    ) => {
        EditorDefinition {
            id: EditorId::$variant,
            label: $label,
            commands: Some(&[$($cmd),*]),
            base_args: &[$($base),*],
            launch_style: EditorLaunchStyle::$style,
            remote_scheme: $remote,
            jetbrains_product_code: $jetbrains,
        }
    };
}

/// Upstream `EDITORS`, in the same order.
pub const EDITORS: &[EditorDefinition] = &[
    editor!(
        Cursor = "Cursor",
        ["cursor"],
        ["--classic"],
        Goto,
        Some("cursor"),
        None
    ),
    // File and workspace opens must target the IDE even when the Agents
    // Window is active.
    editor!(Trae = "Trae", ["trae"], [], Goto, None, None),
    editor!(Kiro = "Kiro", ["kiro"], ["ide"], Goto, None, None),
    editor!(Vscode = "VS Code", ["code"], [], Goto, Some("vscode"), None),
    editor!(
        VscodeInsiders = "VS Code Insiders",
        ["code-insiders"],
        [],
        Goto,
        Some("vscode-insiders"),
        None
    ),
    editor!(
        Vscodium = "VSCodium",
        ["codium"],
        [],
        Goto,
        Some("vscodium"),
        None
    ),
    editor!(
        Zed = "Zed",
        ["zed", "zeditor"],
        [],
        DirectPath,
        Some("zed"),
        None
    ),
    // `agy` is the standalone Antigravity CLI, not the IDE. The IDE bundle
    // ships `antigravity-ide`, so it comes first for install-folder lookups.
    editor!(
        Antigravity = "Antigravity",
        ["antigravity-ide", "agy-ide"],
        [],
        Goto,
        None,
        None
    ),
    editor!(
        Idea = "IntelliJ IDEA",
        ["idea"],
        [],
        LineColumn,
        None,
        Some("IU")
    ),
    editor!(Aqua = "Aqua", ["aqua"], [], LineColumn, None, Some("QA")),
    editor!(Clion = "CLion", ["clion"], [], LineColumn, None, Some("CL")),
    editor!(
        Datagrip = "DataGrip",
        ["datagrip"],
        [],
        LineColumn,
        None,
        Some("DB")
    ),
    editor!(
        Dataspell = "DataSpell",
        ["dataspell"],
        [],
        LineColumn,
        None,
        Some("DS")
    ),
    editor!(
        Goland = "GoLand",
        ["goland"],
        [],
        LineColumn,
        None,
        Some("GO")
    ),
    editor!(
        Phpstorm = "PhpStorm",
        ["phpstorm"],
        [],
        LineColumn,
        None,
        Some("PS")
    ),
    editor!(
        Pycharm = "PyCharm",
        ["pycharm"],
        [],
        LineColumn,
        None,
        Some("PY")
    ),
    editor!(Rider = "Rider", ["rider"], [], LineColumn, None, Some("RD")),
    editor!(
        Rubymine = "RubyMine",
        ["rubymine"],
        [],
        LineColumn,
        None,
        Some("RM")
    ),
    editor!(
        Rustrover = "RustRover",
        ["rustrover"],
        [],
        LineColumn,
        None,
        Some("RR")
    ),
    editor!(
        Webstorm = "WebStorm",
        ["webstorm"],
        [],
        LineColumn,
        None,
        Some("WS")
    ),
    EditorDefinition {
        id: EditorId::FileManager,
        label: "File Manager",
        commands: None,
        base_args: &[],
        launch_style: EditorLaunchStyle::DirectPath,
        remote_scheme: None,
        jetbrains_product_code: None,
    },
];

/// The known editor ids (upstream `Schema.Literals(EDITORS.map(e => e.id))`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EditorId {
    #[serde(rename = "cursor")]
    Cursor,
    #[serde(rename = "trae")]
    Trae,
    #[serde(rename = "kiro")]
    Kiro,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "vscode-insiders")]
    VscodeInsiders,
    #[serde(rename = "vscodium")]
    Vscodium,
    #[serde(rename = "zed")]
    Zed,
    #[serde(rename = "antigravity")]
    Antigravity,
    #[serde(rename = "idea")]
    Idea,
    #[serde(rename = "aqua")]
    Aqua,
    #[serde(rename = "clion")]
    Clion,
    #[serde(rename = "datagrip")]
    Datagrip,
    #[serde(rename = "dataspell")]
    Dataspell,
    #[serde(rename = "goland")]
    Goland,
    #[serde(rename = "phpstorm")]
    Phpstorm,
    #[serde(rename = "pycharm")]
    Pycharm,
    #[serde(rename = "rider")]
    Rider,
    #[serde(rename = "rubymine")]
    Rubymine,
    #[serde(rename = "rustrover")]
    Rustrover,
    #[serde(rename = "webstorm")]
    Webstorm,
    #[serde(rename = "file-manager")]
    FileManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileManagerRevealKind {
    Finder,
    FileExplorer,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchEditorInput {
    pub cwd: TrimmedNonEmptyString,
    pub editor: EditorId,
    /// Reveal (select) `cwd` in the file manager instead of opening it. Only
    /// honored by the "file-manager" editor; clients must check the server's
    /// `shellRevealInFileManager` config flag before sending this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<bool>,
}

fn remote_scheme_of(editor: &EditorDefinition) -> Option<&'static str> {
    editor
        .remote_scheme
        .or(match editor.jetbrains_product_code {
            Some(_) => Some("jetbrains"),
            None => None,
        })
}

fn editor_definition(id: EditorId) -> Option<&'static EditorDefinition> {
    EDITORS.iter().find(|candidate| candidate.id == id)
}

/// Editors that can open a remote workspace via an SSH deep link.
pub fn remote_capable_editor_ids() -> Vec<EditorId> {
    EDITORS
        .iter()
        .filter(|editor| remote_scheme_of(editor).is_some())
        .map(|editor| editor.id)
        .collect()
}

pub fn remote_scheme_for_editor(id: EditorId) -> Option<&'static str> {
    editor_definition(id).and_then(remote_scheme_of)
}

/// `encodeURIComponent`: keep `[A-Za-z0-9-_.!~*'()]`, percent-encode the rest.
fn uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `application/x-www-form-urlencoded` serialization, as
/// `new URLSearchParams(...).toString()` produces: space becomes `+`, and
/// only `[A-Za-z0-9*-._]` stay literal.
fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    fn encode_into(value: &str, out: &mut String) {
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                    out.push(byte as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
    }

    let mut out = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        encode_into(name, &mut out);
        out.push('=');
        encode_into(value, &mut out);
    }
    out
}

/// Builds a `<scheme>://vscode-remote/ssh-remote+<host><path>` deep link (Zed
/// takes `zed://ssh/<host><path>`, JetBrains IDEs a Toolbox App
/// `jetbrains://gateway/ssh/environment?...` link) that opens
/// `absolute_path` on `host` in the local editor over SSH. Returns `None`
/// for editors without remote deep-link support.
pub fn build_remote_open_url(editor: EditorId, host: &str, absolute_path: &str) -> Option<String> {
    let scheme = remote_scheme_for_editor(editor)?;
    if let Some(definition) = editor_definition(editor)
        && let Some(product_code) = definition.jetbrains_product_code
    {
        // Like the VS Code link, no user or port: the SSH config entry
        // for `host` supplies them. A bare product code lets Toolbox pick
        // the backend build.
        let project_hint = absolute_path.replace('\\', "/");
        let query = form_urlencoded(&[
            ("h", host),
            ("launchIde", "true"),
            ("ideHint", product_code),
            ("projectHint", &project_hint),
        ]);
        return Some(format!("{scheme}://gateway/ssh/environment?{query}"));
    }
    // Windows server paths (`C:\...`) appear as `/C:/...` in vscode-remote URIs.
    let posix_path = absolute_path.replace('\\', "/");
    let rooted_path = if posix_path.starts_with('/') {
        posix_path.clone()
    } else {
        format!("/{posix_path}")
    };
    let encoded_host = uri_component(host);
    if editor == EditorId::Zed {
        // Zed's remote server resolves a rooted path on the system drive, so
        // a Windows `C:\Users\x` must become `/Users/x`. Other drives are
        // untested and kept as is rather than silently remapped, and a POSIX
        // path that happens to start with `/C:` is left alone.
        let zed_path = if regex_drive_prefix(absolute_path) {
            rooted_path[3..].to_owned()
        } else {
            rooted_path
        };
        let encoded_zed_path = zed_path
            .split('/')
            .map(uri_component)
            .collect::<Vec<_>>()
            .join("/");
        return Some(format!("{scheme}://ssh/{encoded_host}{encoded_zed_path}"));
    }
    let encoded_path = rooted_path
        .split('/')
        .map(uri_component)
        .collect::<Vec<_>>()
        .join("/");
    Some(format!(
        "{scheme}://vscode-remote/ssh-remote+{encoded_host}{encoded_path}"
    ))
}

/// `^[Cc]:[\\/]` — a Windows drive-letter prefix.
fn regex_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && (bytes[0] == b'C' || bytes[0] == b'c')
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// SSH hostnames an environment advertises for remote open links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteOpenTargetKind {
    Tailscale,
    Mdns,
}

/// Reachability is client-side; the server only advertises names that
/// resolve to itself and gates them on a local sshd listen check. Ordered
/// most-reachable first (tailnet MagicDNS name, then mDNS `<hostname>.local`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOpenTarget {
    pub kind: RemoteOpenTargetKind,
    pub host: TrimmedNonEmptyString,
}

/// `ExternalLauncherUnknownEditorError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherUnknownEditorError {
    #[serde(rename = "ExternalLauncherUnknownEditorError")]
    #[error("Unknown editor: {editor}")]
    UnknownEditor { editor: String },
}

/// `ExternalLauncherUnsupportedEditorError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherUnsupportedEditorError {
    #[serde(rename = "ExternalLauncherUnsupportedEditorError")]
    #[error("Unsupported editor: {editor}")]
    UnsupportedEditor { editor: EditorId },
}

/// `ExternalLauncherCommandNotFoundError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherCommandNotFoundError {
    #[serde(rename = "ExternalLauncherCommandNotFoundError")]
    #[error("Editor command not found: {command}")]
    CommandNotFound { editor: EditorId, command: String },
}

/// The target path cannot reach the editor intact, so it was not started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherUnsupportedTargetError {
    #[serde(rename = "ExternalLauncherUnsupportedTargetError")]
    #[error("{editor} cannot open a path containing line breaks or double quotes")]
    UnsupportedTarget { editor: EditorId },
}

/// `ExternalLauncherBrowserSpawnError` (Schema.TaggedError). Display mirrors
/// upstream's message getter, which joins the command and args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherBrowserSpawnError {
    #[serde(rename = "ExternalLauncherBrowserSpawnError")]
    BrowserSpawn {
        command: String,
        args: Vec<String>,
        cause: serde_json::Value,
        target: String,
    },
}

impl std::fmt::Display for ExternalLauncherBrowserSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrowserSpawn {
                command,
                args,
                target,
                ..
            } => write!(
                f,
                "Failed to launch browser target '{target}' with '{}'",
                [command.clone()]
                    .into_iter()
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }
}

impl std::error::Error for ExternalLauncherBrowserSpawnError {}

/// `ExternalLauncherEditorSpawnError` (Schema.TaggedError). Display mirrors
/// upstream's message getter, which joins the command and args.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ExternalLauncherEditorSpawnError {
    #[serde(rename = "ExternalLauncherEditorSpawnError")]
    EditorSpawn {
        command: String,
        args: Vec<String>,
        cause: serde_json::Value,
        editor: EditorId,
        target: String,
    },
}

impl std::fmt::Display for ExternalLauncherEditorSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EditorSpawn {
                command,
                args,
                editor,
                target,
                ..
            } => write!(
                f,
                "Failed to launch '{target}' in {editor} with '{}'",
                [command.clone()]
                    .into_iter()
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        }
    }
}

impl std::error::Error for ExternalLauncherEditorSpawnError {}

/// `ExternalLauncherError`: union of the launcher's tagged errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum ExternalLauncherError {
    #[error(transparent)]
    UnknownEditor(ExternalLauncherUnknownEditorError),
    #[error(transparent)]
    UnsupportedEditor(ExternalLauncherUnsupportedEditorError),
    #[error(transparent)]
    CommandNotFound(ExternalLauncherCommandNotFoundError),
    #[error(transparent)]
    UnsupportedTarget(ExternalLauncherUnsupportedTargetError),
    #[error(transparent)]
    BrowserSpawn(ExternalLauncherBrowserSpawnError),
    #[error(transparent)]
    EditorSpawn(ExternalLauncherEditorSpawnError),
}

impl std::fmt::Display for EditorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cursor => "cursor",
            Self::Trae => "trae",
            Self::Kiro => "kiro",
            Self::Vscode => "vscode",
            Self::VscodeInsiders => "vscode-insiders",
            Self::Vscodium => "vscodium",
            Self::Zed => "zed",
            Self::Antigravity => "antigravity",
            Self::Idea => "idea",
            Self::Aqua => "aqua",
            Self::Clion => "clion",
            Self::Datagrip => "datagrip",
            Self::Dataspell => "dataspell",
            Self::Goland => "goland",
            Self::Phpstorm => "phpstorm",
            Self::Pycharm => "pycharm",
            Self::Rider => "rider",
            Self::Rubymine => "rubymine",
            Self::Rustrover => "rustrover",
            Self::Webstorm => "webstorm",
            Self::FileManager => "file-manager",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_open_urls_match_upstream_shapes() {
        // VS Code-style deep link.
        let url = build_remote_open_url(EditorId::Vscode, "box.tail1234.ts.net", "/home/dev/repo")
            .unwrap();
        assert_eq!(
            url,
            "vscode://vscode-remote/ssh-remote+box.tail1234.ts.net/home/dev/repo"
        );

        // Zed trims the drive letter of a Windows system-drive path.
        let zed = build_remote_open_url(EditorId::Zed, "host", "C:\\Users\\x").unwrap();
        assert_eq!(zed, "zed://ssh/host/Users/x");

        // Other Windows drives keep their rooted shape; `encodeURIComponent`
        // also encodes the `:` of a non-system drive.
        let zed_d = build_remote_open_url(EditorId::Zed, "host", "D:\\work").unwrap();
        assert_eq!(zed_d, "zed://ssh/host/D%3A/work");

        // JetBrains goes through the Toolbox App link.
        let jb = build_remote_open_url(EditorId::Idea, "box", "/srv/app").unwrap();
        assert_eq!(
            jb,
            "jetbrains://gateway/ssh/environment?h=box&launchIde=true&ideHint=IU&projectHint=%2Fsrv%2Fapp"
        );

        // Editors without remote support return None.
        assert_eq!(
            build_remote_open_url(EditorId::FileManager, "box", "/x"),
            None
        );
        assert_eq!(build_remote_open_url(EditorId::Trae, "box", "/x"), None);
    }

    #[test]
    fn remote_capable_editors() {
        assert!(remote_capable_editor_ids().contains(&EditorId::Cursor));
        assert!(remote_capable_editor_ids().contains(&EditorId::Zed));
        assert!(remote_capable_editor_ids().contains(&EditorId::Idea));
        assert!(!remote_capable_editor_ids().contains(&EditorId::FileManager));
        assert_eq!(remote_scheme_for_editor(EditorId::Idea), Some("jetbrains"));
    }

    #[test]
    fn uri_component_matches_encode_uri_component() {
        assert_eq!(uri_component("a b/c"), "a%20b%2Fc");
        assert_eq!(uri_component("ü"), "%C3%BC");
    }

    #[test]
    fn launcher_error_union_decodes_by_tag_and_formats_messages() {
        use serde_json::json;
        let samples = [
            (json!({ "_tag": "ExternalLauncherUnknownEditorError", "editor": "emacs" }), "Unknown editor: emacs"),
            (json!({ "_tag": "ExternalLauncherUnsupportedEditorError", "editor": "zed" }), "Unsupported editor: zed"),
            (
                json!({ "_tag": "ExternalLauncherCommandNotFoundError", "editor": "zed", "command": "zed" }),
                "Editor command not found: zed",
            ),
            (
                json!({ "_tag": "ExternalLauncherUnsupportedTargetError", "editor": "cursor" }),
                "cursor cannot open a path containing line breaks or double quotes",
            ),
            (
                json!({ "_tag": "ExternalLauncherBrowserSpawnError", "command": "open", "args": ["-a", "x"], "cause": "e", "target": "https://a" }),
                "Failed to launch browser target 'https://a' with 'open -a x'",
            ),
            (
                json!({ "_tag": "ExternalLauncherEditorSpawnError", "command": "code", "args": ["--goto"], "cause": null, "editor": "vscode", "target": "/p" }),
                "Failed to launch '/p' in vscode with 'code --goto'",
            ),
        ];
        for (sample, message) in samples {
            let error: ExternalLauncherError = serde_json::from_value(sample.clone()).unwrap();
            assert_eq!(error.to_string(), message);
            assert_eq!(serde_json::to_value(&error).unwrap(), sample);
        }
        assert!(serde_json::from_value::<ExternalLauncherError>(
            json!({ "_tag": "ExternalLauncherUnsupportedEditorError", "editor": "emacs" })
        )
        .is_err());
    }

    #[test]
    fn launch_input_reveal_is_optional() {
        let input: LaunchEditorInput =
            serde_json::from_value(serde_json::json!({ "cwd": " /p ", "editor": "file-manager" })).unwrap();
        assert_eq!(input.cwd.as_str(), "/p");
        assert_eq!(input.reveal, None);
        assert!(serde_json::from_value::<LaunchEditorInput>(serde_json::json!({ "cwd": "/p", "editor": "emacs" })).is_err());
    }
}
