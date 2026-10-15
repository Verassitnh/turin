//! Port of upstream/apps/server/src/provider/Drivers/CodexHomeLayout.ts, the
//! part discovery reads: where the shared Codex home (sessions, transcripts)
//! lives. Shadow-home materialization (`CodexShadowHome*`) belongs to the
//! Codex provider and is not ported here.

use std::path::{Component, Path, PathBuf};

use crate::project_path::expand_home_path;

/// Upstream `CodexHomeLayout.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexHomeMode {
    Direct,
    AuthOverlay,
}

/// Upstream `CodexHomeLayout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHomeLayout {
    pub mode: CodexHomeMode,
    pub shared_home_path: String,
    /// `None` in direct mode when `homePath` is unset (Codex then uses its own default).
    pub effective_home_path: Option<String>,
    pub continuation_key: String,
}

/// Upstream `resolveCodexHomeLayout`. `home_path` and `shadow_home_path` are
/// the raw settings values; `home` is `HostProcess.HomeDirectory`.
pub fn resolve_codex_home_layout(
    home: &str,
    home_path: &str,
    shadow_home_path: &str,
) -> CodexHomeLayout {
    let shared_home_path = resolve_home_path(home, home_path);
    let continuation_key = format!("codex:home:{shared_home_path}");
    let shadow = shadow_home_path.trim();
    if shadow.is_empty() {
        let effective = (!home_path.trim().is_empty()).then(|| shared_home_path.clone());
        return CodexHomeLayout {
            mode: CodexHomeMode::Direct,
            shared_home_path,
            effective_home_path: effective,
            continuation_key,
        };
    }
    CodexHomeLayout {
        mode: CodexHomeMode::AuthOverlay,
        shared_home_path,
        effective_home_path: Some(lexical_resolve(&expand_home_path(shadow, home))),
        continuation_key,
    }
}

fn resolve_home_path(home: &str, value: &str) -> String {
    let expanded = if value.trim().is_empty() {
        Path::new(home)
            .join(".codex")
            .to_string_lossy()
            .into_owned()
    } else {
        expand_home_path(value, home)
    };
    lexical_resolve(&expanded)
}

/// Node `path.resolve` for an absolute path: `.` and `..` are folded, no
/// filesystem access. Relative input resolves against the process cwd.
pub(crate) fn lexical_resolve(value: &str) -> String {
    let absolute = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(value))
            .unwrap_or_else(|_| PathBuf::from(value))
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}
