//! Port of upstream/packages/provider-core/src/server/pathExpansion.ts.

use std::path::Path;

/// Expand a leading `~`, `~/`, or `~\` using the supplied home directory.
/// Other-user expansion is deliberately unsupported.
pub fn expand_home_path(value: &str, home: &str) -> String {
    if value == "~" {
        return home.to_owned();
    }
    if let Some(tail) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return Path::new(home).join(tail).to_string_lossy().into_owned();
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_cases() {
        let home = "/home/ada";
        for path in [
            "",
            "/absolute/path",
            "relative/path",
            "some~weird~path",
            "~alice/foo",
        ] {
            assert_eq!(expand_home_path(path, home), path);
        }
        assert_eq!(expand_home_path("~", home), home);
        assert_eq!(
            expand_home_path("~/.codex-work", home),
            "/home/ada/.codex-work"
        );
        assert_eq!(expand_home_path("~\\.codex", home), "/home/ada/.codex");
    }
}
