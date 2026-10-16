//! Port of upstream/apps/server/src/provider/Drivers/ClaudeHome.ts.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
use turin_provider_core::{host_process::Platform, path_expansion::expand_home_path};

/// `ProcessEnv`, the same map `turin_provider_core::host_process` uses.
pub type ProcessEnv = HashMap<String, String>;

/// `resolveClaudeHomePath`: the instance's `homePath`, then an inherited
/// `CLAUDE_CONFIG_DIR`, then Claude's default `~/.claude`. An empty `homePath`
/// must not fall back to bare `$HOME`, which names a different continuation
/// group than an explicit `~/.claude`.
pub fn resolve_claude_home_path(
    home_path: &str,
    environment: Option<&ProcessEnv>,
    home: &str,
) -> PathBuf {
    let home_path = home_path.trim();
    if !home_path.is_empty() {
        return absolute(Path::new(&expand_home_path(home_path, home)));
    }
    // Inherited env vars are not shell-expanded, so a literal `~` stays literal.
    let inherited = environment
        .and_then(|environment| environment.get("CLAUDE_CONFIG_DIR"))
        .map(|value| value.trim())
        .unwrap_or("");
    if !inherited.is_empty() {
        return absolute(Path::new(inherited));
    }
    absolute(&Path::new(home).join(".claude"))
}

/// `makeClaudeEnvironment`: with a `homePath`, the child gets
/// `CLAUDE_CONFIG_DIR` and keeps `HOME`, so the macOS login keychain lookup
/// still finds the stored OAuth credentials. Without one, the base env is used as-is.
pub fn make_claude_environment(home_path: &str, base_env: &ProcessEnv, home: &str) -> ProcessEnv {
    if home_path.trim().is_empty() {
        return base_env.clone();
    }
    let mut environment = base_env.clone();
    environment.insert(
        "CLAUDE_CONFIG_DIR".to_owned(),
        resolve_claude_home_path(home_path, None, home)
            .to_string_lossy()
            .into_owned(),
    );
    environment
}

/// `hasClaudeManagedMcpConfig`: an administrator's `managed-mcp.json`. While it
/// exists Claude refuses `--strict-mcp-config`, so callers drop the flag.
pub async fn has_claude_managed_mcp_config(platform: Platform) -> bool {
    let path = match platform {
        Platform::Darwin => "/Library/Application Support/ClaudeCode/managed-mcp.json",
        Platform::Win32 => "C:\\Program Files\\ClaudeCode\\managed-mcp.json",
        _ => "/etc/claude-code/managed-mcp.json",
    };
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// `makeClaudeContinuationGroupKey`.
pub fn make_claude_continuation_group_key(
    home_path: &str,
    environment: Option<&ProcessEnv>,
    home: &str,
) -> String {
    format!(
        "claude:home:{}",
        resolve_claude_home_path(home_path, environment, home).to_string_lossy()
    )
}

/// `makeClaudeCapabilitiesCacheKey`: binary, resolved home, and cwd, so
/// account-specific probes never share auth metadata across instances.
pub fn make_claude_capabilities_cache_key(
    binary_path: &str,
    home_path: &str,
    cwd: Option<&str>,
    environment: Option<&ProcessEnv>,
    home: &str,
) -> String {
    format!(
        "{binary_path}\0{}\0{}",
        resolve_claude_home_path(home_path, environment, home).to_string_lossy(),
        cwd.unwrap_or("")
    )
}

/// `path.resolve`: a relative path is anchored at the process cwd.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ProcessEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn home_path_wins_then_inherited_config_dir_then_default() {
        let inherited = env(&[("CLAUDE_CONFIG_DIR", "/inherited/claude")]);
        assert_eq!(
            resolve_claude_home_path("~/work-claude", Some(&inherited), "/Users/dev"),
            PathBuf::from("/Users/dev/work-claude")
        );
        assert_eq!(
            resolve_claude_home_path("  ", Some(&inherited), "/Users/dev"),
            PathBuf::from("/inherited/claude")
        );
        assert_eq!(
            resolve_claude_home_path("", Some(&env(&[])), "/Users/dev"),
            PathBuf::from("/Users/dev/.claude")
        );
    }

    #[test]
    fn instance_home_is_exported_as_config_dir_and_keeps_home() {
        let base = env(&[("HOME", "/Users/dev"), ("CLAUDE_CONFIG_DIR", "/old")]);
        let environment = make_claude_environment("/srv/claude", &base, "/Users/dev");
        assert_eq!(environment["CLAUDE_CONFIG_DIR"], "/srv/claude");
        assert_eq!(environment["HOME"], "/Users/dev");
        assert_eq!(make_claude_environment("", &base, "/Users/dev"), base);
    }

    #[test]
    fn cache_key_separates_binary_home_and_cwd() {
        let key = make_claude_capabilities_cache_key("claude", "/a", Some("/w"), None, "/h");
        assert_eq!(key, "claude\0/a\0/w");
        assert_eq!(
            make_claude_continuation_group_key("/a", None, "/h"),
            "claude:home:/a"
        );
    }
}
