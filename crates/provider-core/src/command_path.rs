//! Resolve an executable on `PATH` (`resolveCommandPath` in
//! `@t3tools/shared/shell`).
//!
//! Local shim covering what provider-core needs: explicit paths and `PATH`
//! scans with `PATHEXT` handling. Upstream additionally memoizes scans and
//! lists `PATH` directories once per batch; both are performance caches that
//! this shim omits. Swap for `turin_shared::shell::resolve_command_path` when
//! it lands.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::host_process::{Platform, ProcessEnv};

fn read_env_path(env: &ProcessEnv) -> Option<&str> {
    env.get("PATH")
        .or_else(|| env.get("Path"))
        .or_else(|| env.get("path"))
        .map(String::as_str)
}

fn windows_path_extensions(env: &ProcessEnv) -> Vec<String> {
    let fallback = || vec![".COM".into(), ".EXE".into(), ".BAT".into(), ".CMD".into()];
    let Some(raw) = env.get("PATHEXT").filter(|raw| !raw.is_empty()) else {
        return fallback();
    };
    let mut parsed: Vec<String> = Vec::new();
    for entry in raw.split(';') {
        let trimmed = turin_contracts::js_trim(entry);
        if trimmed.is_empty() {
            continue;
        }
        let extension = if trimmed.starts_with('.') {
            trimmed.to_uppercase()
        } else {
            format!(".{}", trimmed.to_uppercase())
        };
        if !parsed.contains(&extension) {
            parsed.push(extension);
        }
    }
    if parsed.is_empty() {
        fallback()
    } else {
        parsed
    }
}

fn extname(command: &str) -> &str {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    match name.rfind('.') {
        Some(index) if index > 0 => &name[index..],
        _ => "",
    }
}

fn command_candidates(command: &str, platform: Platform, extensions: &[String]) -> Vec<String> {
    if platform != Platform::Win32 {
        return vec![command.to_owned()];
    }
    let extension = extname(command);
    let upper = extension.to_uppercase();
    let mut out: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !out.contains(&value) {
            out.push(value);
        }
    };
    if !extension.is_empty() && extensions.contains(&upper) {
        let base = &command[..command.len() - extension.len()];
        push(command.to_owned());
        push(format!("{base}{upper}"));
        push(format!("{base}{}", upper.to_lowercase()));
        return out;
    }
    for candidate_extension in extensions {
        push(format!("{command}{candidate_extension}"));
        push(format!("{command}{}", candidate_extension.to_lowercase()));
    }
    out
}

#[cfg(unix)]
fn can_execute(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn can_execute(_path: &Path) -> bool {
    true
}

fn is_executable_file(path: &Path, platform: Platform, extensions: &[String]) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    if platform == Platform::Win32 {
        let extension = path
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy().to_uppercase()));
        return extension.is_some_and(|extension| extensions.contains(&extension));
    }
    can_execute(path)
}

fn strip_wrapping_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// The absolute path of the executable `command` names under `env`, or `None`.
/// A command containing a path separator is checked in place and never searched.
pub fn resolve_command_path(command: &str, env: &ProcessEnv, platform: Platform) -> Option<String> {
    let extensions = if platform == Platform::Win32 {
        windows_path_extensions(env)
    } else {
        Vec::new()
    };
    let candidates = command_candidates(command, platform, &extensions);

    if command.contains('/') || command.contains('\\') {
        return candidates
            .into_iter()
            .find(|candidate| is_executable_file(Path::new(candidate), platform, &extensions));
    }

    let path_value = read_env_path(env).unwrap_or("");
    if path_value.is_empty() {
        return None;
    }
    let delimiter = if platform == Platform::Win32 {
        ';'
    } else {
        ':'
    };
    let mut seen = HashSet::new();
    for entry in path_value.split(delimiter) {
        let entry = strip_wrapping_quotes(turin_contracts::js_trim(entry));
        if entry.is_empty() || !seen.insert(entry.to_owned()) {
            continue;
        }
        for candidate in &candidates {
            let full: PathBuf = Path::new(entry).join(candidate);
            if is_executable_file(&full, platform, &extensions) {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn finds_executables_on_path_and_by_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "x").unwrap();

        let env = ProcessEnv::from([(
            "PATH".to_owned(),
            format!("/definitely/missing:{}", dir.path().display()),
        )]);
        assert_eq!(
            resolve_command_path("tool", &env, Platform::Darwin),
            Some(exe.to_string_lossy().into_owned())
        );
        assert_eq!(resolve_command_path("plain", &env, Platform::Darwin), None);
        assert_eq!(
            resolve_command_path(exe.to_str().unwrap(), &ProcessEnv::new(), Platform::Darwin),
            Some(exe.to_string_lossy().into_owned())
        );
        assert_eq!(
            resolve_command_path("tool", &ProcessEnv::new(), Platform::Darwin),
            None
        );
    }

    #[test]
    fn windows_candidates_follow_pathext() {
        let env = ProcessEnv::from([("PATHEXT".to_owned(), "exe;.CMD".to_owned())]);
        let extensions = windows_path_extensions(&env);
        assert_eq!(extensions, [".EXE", ".CMD"]);
        assert_eq!(
            command_candidates("claude", Platform::Win32, &extensions),
            ["claude.EXE", "claude.exe", "claude.CMD", "claude.cmd"]
        );
        assert_eq!(
            command_candidates("claude.cmd", Platform::Win32, &extensions),
            ["claude.cmd", "claude.CMD"]
        );
    }
}
