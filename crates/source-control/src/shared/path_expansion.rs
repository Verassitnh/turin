//! Port of upstream/packages/provider-core/src/server/pathExpansion.ts
//!
//! Expand a leading `~` (or `~/…`, `~\…`) in a user-supplied path to the current user's home
//! directory. Spawned processes get no shell expansion, so a value such as `~/code` would reach
//! git verbatim and be read as a relative path. Does not handle `~user` (other-user) expansion.

use std::path::Path;

/// Returns `value` with a leading `~` replaced by `home`; anything else, including the empty
/// string, comes back unchanged.
pub fn expand_home_path(value: &str, home: &str) -> String {
    if value.is_empty() {
        return value.to_owned();
    }
    if value == "~" {
        return home.to_owned();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return Path::new(home).join(rest).to_string_lossy().into_owned();
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_a_leading_tilde_and_leaves_other_paths_alone() {
        assert_eq!(expand_home_path("~", "/home/me"), "/home/me");
        assert_eq!(
            expand_home_path("~/code/repo", "/home/me"),
            "/home/me/code/repo"
        );
        assert_eq!(expand_home_path("~\\code", "/home/me"), "/home/me/code");
        assert_eq!(expand_home_path("~other/code", "/home/me"), "~other/code");
        assert_eq!(expand_home_path("/srv/code", "/home/me"), "/srv/code");
        assert_eq!(expand_home_path("", "/home/me"), "");
    }
}
