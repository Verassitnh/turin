//! Upstream `expandHomePath` only. The `normalizeProjectPath*` helpers of
//! upstream/packages/shared/src/path.ts are `turin_shared::path`; see PORTING.md, DEDUP.

/// Upstream `expandHomePath` (`packages/provider-core/src/server/pathExpansion.ts`).
/// Expands a leading `~`, `~/…`, or `~\…` to `home`. Spawned processes get no
/// shell expansion, so `CODEX_HOME=~/.codex-work` would otherwise stay literal.
/// `~user` is not expanded.
pub fn expand_home_path(value: &str, home: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value == "~" {
        return home.to_owned();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return std::path::Path::new(home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    value.to_owned()
}
